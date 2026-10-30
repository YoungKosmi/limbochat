//! Narzędzia Jarvisa: deklaracje dla Gemini i ich wykonanie.
//!
//! Zasady bezpieczeństwa egzekwowane w kodzie (nie tylko w instrukcji dla AI):
//! * ban, wyczyszczenie czatu, raid i reklama NIGDY nie wykonują się od razu.
//!   Pierwsze wywołanie tylko zapamiętuje akcję i każe zapytać o zgodę;
//!   wykonuje ją dopiero `confirm_pending(true)` w KOLEJNEJ komendzie głosowej,
//!   najpóźniej po 60 s. Przekręcony nick nie skończy się więc przypadkowym banem.
//! * Ustawienia streamu (kategoria, tytuł, klip, raid…) dotyczą zawsze Twojego
//!   kanału; moderacja czatu — kanału otwartego w panelu.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::twitch::{CHAT_COLORS, ChatMode, PredictionEnd, normalize};
use crate::{events::ChannelRef, helix::Helix, hub::HubHandle};

const CONFIRM_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Chatter {
    pub name: String,
    pub login: String,
    pub id: String,
}

/// Migawka stanu w chwili komendy (z huba panelu).
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Zalogowany użytkownik (login, id) — jego kanał to „Twój kanał”.
    pub me: Option<(String, String)>,
    /// Kanał otwarty w panelu.
    pub active: Option<ChannelRef>,
    pub can_moderate_active: bool,
    pub chatters: Vec<Chatter>,
    /// Ostatnie wiadomości: (nick, treść).
    pub recent: Vec<(String, String)>,
    pub demo: bool,
    /// Stan OBS (gdy sterowanie OBS jest włączone).
    pub obs: Option<crate::obs::ObsStatus>,
    /// Pamięć o osobach z czatu: (nick, krótki opis).
    pub memory: Vec<(String, String)>,
    /// Pobrana, gotowa do instalacji nowa wersja LiMBOCHAT: (numer, lista zmian).
    pub update: Option<(String, String)>,
}

impl Context {
    fn own(&self) -> Option<ChannelRef> {
        self.me.as_ref().map(|(login, id)| ChannelRef { login: login.clone(), id: id.clone() })
    }

    /// Kanał do moderacji czatu: otwarty w panelu, a jeśli brak — własny.
    fn chat_channel(&self) -> Option<ChannelRef> {
        self.active.clone().or_else(|| self.own())
    }
}

/// Efekt widoczny w panelu (chip akcji).
#[derive(Debug, Clone)]
pub struct Action {
    pub text: String,
    pub ok: bool,
    pub url: Option<String>,
}

pub struct Outcome {
    pub result: Value,
    pub action: Option<Action>,
}

struct Pending {
    name: String,
    args: Value,
    at: Instant,
    label: String,
}

/// Stan streamu w trybie demo (bez Twitcha).
struct DemoStream {
    title: String,
    category: String,
}

/// Działania poza Twitchem: panel (kolor, kanały) i przypomnienia.
#[derive(Clone, Default)]
pub struct Local {
    pub hub: Option<HubHandle>,
    pub reminders: Option<tokio::sync::mpsc::UnboundedSender<ReminderCmd>>,
    pub obs: Option<crate::obs::ObsHandle>,
    pub memory: Option<Arc<crate::memory::Memory>>,
}

#[derive(Debug)]
pub enum ReminderCmd {
    Add { after: Duration, text: String },
    CancelAll,
}

pub struct Executor {
    helix: Option<Arc<Helix>>,
    local: Local,
    pending: Option<Pending>,
    demo: DemoStream,
}

impl Executor {
    pub fn new(helix: Option<Arc<Helix>>, local: Local) -> Self {
        Self {
            helix,
            local,
            pending: None,
            demo: DemoStream { title: "Wieczorne granie z czatem".into(), category: "Just Chatting".into() },
        }
    }

    /// Opis oczekującej akcji (do kontekstu dla AI).
    pub fn pending_label(&self) -> Option<String> {
        self.pending.as_ref().filter(|p| p.at.elapsed() < CONFIRM_WINDOW).map(|p| p.label.clone())
    }

    pub async fn run(&mut self, ctx: &Context, name: &str, args: &Value) -> Outcome {
        if is_destructive(name) {
            let label = describe_pending(name, args);
            self.pending = Some(Pending { name: name.to_owned(), args: args.clone(), at: Instant::now(), label: label.clone() });
            return Outcome {
                result: json!({
                    "ok": false,
                    "status": "needs_confirmation",
                    "pending": label,
                    "instruction": "NIE wykonano jeszcze. Zapytaj krótko, czy na pewno. Użytkownik potwierdzi następną komendą głosową."
                }),
                action: Some(Action { text: format!("❔ Czeka na potwierdzenie: {label}"), ok: true, url: None }),
            };
        }

        if name == "confirm_pending" {
            let confirmed = args["confirmed"].as_bool().unwrap_or(false);
            let Some(pending) = self.pending.take() else {
                return fail("Nie ma żadnej akcji czekającej na potwierdzenie.");
            };
            if pending.at.elapsed() > CONFIRM_WINDOW {
                return fail("Potwierdzenie przyszło za późno (ponad minuta) — akcja anulowana. Wydaj polecenie ponownie.");
            }
            if !confirmed {
                return Outcome {
                    result: json!({ "ok": true, "cancelled": pending.label }),
                    action: Some(Action { text: format!("↩ Anulowano: {}", pending.label), ok: true, url: None }),
                };
            }
            return self.execute(ctx, &pending.name, &pending.args).await;
        }

        self.execute(ctx, name, args).await
    }

    async fn execute(&mut self, ctx: &Context, name: &str, args: &Value) -> Outcome {
        if let Some(outcome) = self.execute_local(ctx, name, args).await {
            return outcome;
        }
        if ctx.demo || self.helix.is_none() {
            return self.execute_demo(ctx, name, args);
        }
        let helix = self.helix.clone().expect("sprawdzone wyżej");
        let Some(own) = ctx.own() else {
            return fail("Nie jesteś zalogowany do Twitcha — zaloguj się w panelu LiMBOCHAT.");
        };
        match run_helix(&helix, ctx, &own, name, args).await {
            Ok(outcome) => outcome,
            Err(e) => fail(&e),
        }
    }

    /// Narzędzia niezależne od Twitcha. `None` = to nie jest narzędzie lokalne.
    async fn execute_local(&mut self, ctx: &Context, name: &str, args: &Value) -> Option<Outcome> {
        let s = |key: &str| args[key].as_str().unwrap_or_default().trim().to_owned();
        if matches!(name,"mark_moment"|"obs_save_replay"){
            if let Some(hub)=&self.local.hub{
                let command=crate::studio::Command::Moment{label:s("label"),replay:name=="obs_save_replay"};
                return Some(match hub.studio_command(command).await{Ok(value)=>ok(value,Some(if name=="obs_save_replay"{"💾 Moment zapisany, czekam na potwierdzenie powtórki z OBS".into()}else{"📍 Moment zapisany".into()})),Err(error)=>fail(&error)});
            }
        }
        if name.starts_with("obs_") {
            return Some(match &self.local.obs {
                Some(obs) => run_obs(obs, name, args).await.unwrap_or_else(|e| fail(&e)),
                None => fail("Sterowanie OBS nie jest dostępne."),
            });
        }
        if matches!(name, "remember_viewer" | "forget_viewer" | "viewer_info") {
            let Some(memory) = &self.local.memory else { return Some(fail("Pamięć o widzach jest wyłączona (⚙ → Pamięć o widzach).")) };
            let spoken = s("user");
            let (login, display) = match resolve_known(ctx, &spoken) {
                Some(c) => (c.login, c.name),
                None => match memory.find(&spoken) {
                    Some((login, v)) => (login, v.name),
                    None => {
                        let login: String = spoken.trim_start_matches('@').chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>().to_lowercase();
                        (login.clone(), login)
                    }
                },
            };
            if login.is_empty() {
                return Some(fail(&format!("Nie wiem, kim jest „{spoken}”.")));
            }
            return Some(match name {
                "remember_viewer" => {
                    let fact = s("fact");
                    if memory.remember(&login, &display, &fact, "voice") {
                        ok(json!({ "ok": true, "user": display, "fact": fact }), Some(format!("🧠 Zapamiętano: {display} — {fact}")))
                    } else {
                        ok(json!({ "ok": true, "already_known": true }), None)
                    }
                }
                "forget_viewer" => {
                    let fact = s("fact");
                    let which = if fact.is_empty() { crate::memory::Forget::All } else { crate::memory::Forget::Matching(fact) };
                    let n = memory.forget(&login, which);
                    ok(json!({ "ok": true, "removed": n }), Some(format!("🧠 Zapomniano ({n}) o {display}")))
                }
                _ => match memory.get(&login) {
                    Some(v) => ok(
                        json!({
                            "user": v.name, "notes": v.notes.iter().map(|n| n.text.clone()).collect::<Vec<_>>(),
                            "messages": v.messages, "streams": v.streams, "bits": v.bits, "sub_months": v.sub_months, "gifted": v.gifted,
                            "first_seen": chrono::DateTime::from_timestamp_millis(v.first_seen).map(|d| d.format("%d.%m.%Y").to_string()),
                        }),
                        None,
                    ),
                    None => ok(json!({ "user": display, "known": false }), None),
                },
            });
        }
        Some(match name {
            "set_panel_color" => match panel_color(&s("color")) {
                Some((hex, label)) => {
                    if let Some(hub) = &self.local.hub {
                        hub.set_accent(hex.clone());
                    }
                    ok(json!({ "ok": true, "color": label, "hex": hex }), Some(format!("🎨 Kolor panelu → {label}")))
                }
                None => fail("Nie znam tego koloru. Dostępne: fiolet, róż, czerwień, pomarańcz, złoto, zieleń, turkus, niebieski, srebro albo #RRGGBB."),
            },
            "check_update" => {
                let Some(hub) = &self.local.hub else { return Some(fail("Panel nie jest dostępny.")) };
                match hub.check_update().await {
                    Err(e) => fail(&e),
                    Ok(s) => match s.state.as_str() {
                        "ready" => {
                            let version = s.available.unwrap_or_default();
                            ok(json!({ "new_version": version, "current": s.version, "notes": s.notes, "ask": "zapytaj, czy zainstalować teraz" }), Some(format!("⬆ Jest nowa wersja {version}")))
                        }
                        "up_to_date" => ok(json!({ "up_to_date": true, "current": s.version }), Some(format!("✓ LiMBOCHAT {} jest aktualny", s.version))),
                        "error" => fail(s.error.as_deref().unwrap_or("Nie udało się sprawdzić aktualizacji.")),
                        _ => ok(json!({ "downloading": s.available, "info": "nowa wersja jeszcze się pobiera — LimboAI powie, gdy będzie gotowa" }), None),
                    },
                }
            }
            "install_update" => {
                let Some((version, _)) = &ctx.update else {
                    return Some(fail("Nie ma nowej wersji do zainstalowania — LiMBOCHAT jest aktualny albo aktualizacja jeszcze się pobiera."));
                };
                let Some(hub) = &self.local.hub else { return Some(fail("Panel nie jest dostępny.")) };
                hub.install_update();
                ok(json!({ "ok": true, "version": version, "when": "zaraz po tej odpowiedzi, bez restartu OBS" }), Some(format!("⬆ Instaluję LiMBOCHAT {version}")))
            }
            "open_channel" => {
                let Some(hub) = &self.local.hub else { return Some(fail("Panel nie jest dostępny.")) };
                match hub.open_channel(s("channel")).await {
                    Ok(login) => ok(json!({ "ok": true, "channel": login }), Some(format!("📺 Kanał #{login}"))),
                    Err(e) => fail(&e),
                }
            }
            "set_reminder" => {
                let secs = args["seconds"].as_u64().unwrap_or(0);
                let text = s("text");
                if secs == 0 || secs > 6 * 3600 || text.is_empty() {
                    return Some(fail("Przypomnienie potrzebuje treści i czasu (do 6 godzin)."));
                }
                let Some(tx) = &self.local.reminders else { return Some(fail("Przypomnienia są niedostępne.")) };
                let _ = tx.send(ReminderCmd::Add { after: Duration::from_secs(secs), text: text.clone() });
                ok(json!({ "ok": true, "in": human_duration(secs) }), Some(format!("⏰ Za {}: {text}", human_duration(secs))))
            }
            "cancel_reminders" => {
                if let Some(tx) = &self.local.reminders {
                    let _ = tx.send(ReminderCmd::CancelAll);
                }
                ok(json!({ "ok": true }), Some("⏰ Przypomnienia anulowane".into()))
            }
            "pick_random_chatter" => {
                let exclude: Vec<String> = ctx.me.iter().map(|(l, _)| l.clone()).collect();
                let pool: Vec<&Chatter> = ctx.chatters.iter().filter(|c| !exclude.contains(&c.login)).collect();
                if pool.is_empty() {
                    return Some(fail("Na czacie nikt jeszcze nie pisał."));
                }
                let pick = pool[random_index(pool.len())];
                ok(json!({ "ok": true, "winner": pick.name, "pool": pool.len() }), Some(format!("🎲 Wylosowano: {} (z {})", pick.name, pool.len())))
            }
            _ => return None,
        })
    }

    fn execute_demo(&mut self, ctx: &Context, name: &str, args: &Value) -> Outcome {
        let s = |key: &str| args[key].as_str().unwrap_or_default().to_owned();
        let user = || resolve_known(ctx, &s("user")).map_or_else(|| s("user"), |c| c.name);
        match name {
            "get_stream_info" => ok(
                json!({ "live": true, "title": self.demo.title, "category": self.demo.category, "viewers": 128, "uptime": "1 h 42 min", "followers": 3456 }),
                None,
            ),
            "get_recent_chat" => ok(json!({ "messages": recent_json(ctx, args) }), None),
            "set_category" => {
                self.demo.category = s("name");
                ok(json!({ "ok": true, "category": self.demo.category }), Some(format!("🎮 Kategoria → {}", self.demo.category)))
            }
            "set_title" => {
                self.demo.title = s("title");
                ok(json!({ "ok": true, "title": self.demo.title }), Some("✏️ Nowy tytuł streamu".into()))
            }
            "timeout_user" => {
                let secs = args["seconds"].as_u64().unwrap_or(600);
                ok(json!({ "ok": true, "user": user() }), Some(format!("⏱ Timeout: {} ({})", user(), human_duration(secs))))
            }
            "ban_user" => ok(json!({ "ok": true, "user": user() }), Some(format!("🚫 Ban: {}", user()))),
            "unban_user" => ok(json!({ "ok": true, "user": user() }), Some(format!("✅ Unban: {}", user()))),
            "add_moderator" => ok(json!({ "ok": true, "user": user() }), Some(format!("🛡 Moderator: {}", user()))),
            "remove_moderator" => ok(json!({ "ok": true, "user": user() }), Some(format!("🛡 Odebrano moda: {}", user()))),
            "add_vip" => ok(json!({ "ok": true, "user": user() }), Some(format!("💎 VIP: {}", user()))),
            "remove_vip" => ok(json!({ "ok": true, "user": user() }), Some(format!("💎 Odebrano VIP: {}", user()))),
            "warn_user" => ok(json!({ "ok": true, "user": user() }), Some(format!("⚠ Ostrzeżenie: {}", user()))),
            "poll_results" => ok(
                json!({ "poll": { "title": "Co gramy dalej?", "status": "zakończona (minął czas)", "total_votes": 47, "winner": "Minecraft",
                    "choices": [{ "title": "Minecraft", "votes": 29, "percent": 62 }, { "title": "GTA", "votes": 18, "percent": 38 }] } }),
                None,
            ),
            "prediction_results" => ok(
                json!({ "prediction": { "title": "Wygram tę rundę?", "status": "rozstrzygnięta", "winner": "Tak",
                    "outcomes": [{ "title": "Tak", "users": 21, "channel_points": 18400 }, { "title": "Nie", "users": 15, "channel_points": 9100 }] } }),
                None,
            ),
            _ => ok(json!({ "ok": true, "demo": true }), Some(demo_label(name, args))),
        }
    }
}

async fn run_helix(helix: &Helix, ctx: &Context, own: &ChannelRef, name: &str, args: &Value) -> Result<Outcome, String> {
    let s = |key: &str| args[key].as_str().unwrap_or_default().trim().to_owned();
    let chat = ctx.chat_channel().unwrap_or_else(|| own.clone());
    let e = |err: crate::helix::HelixError| err.to_string();

    Ok(match name {
        "get_stream_info" => {
            let info = helix.channel_info(&own.id).await.map_err(e)?;
            let stream = helix.live_stream(&own.id).await.map_err(e)?;
            let followers = helix.follower_total(&own.id).await.ok();
            let uptime = stream.as_ref().and_then(|st| chrono::DateTime::parse_from_rfc3339(&st.started_at).ok()).map(|start| {
                let secs = (chrono::Utc::now() - start.with_timezone(&chrono::Utc)).num_seconds().max(0) as u64;
                human_duration(secs)
            });
            ok(
                json!({
                    "live": stream.is_some(),
                    "title": info.as_ref().map(|i| i.title.clone()),
                    "category": info.as_ref().map(|i| i.game_name.clone()),
                    "viewers": stream.as_ref().map(|st| st.viewer_count),
                    "uptime": uptime,
                    "followers": followers,
                }),
                None,
            )
        }
        "get_recent_chat" => ok(json!({ "messages": recent_json(ctx, args) }), None),
        "set_category" => {
            let wanted = s("name");
            if wanted.is_empty() {
                return Err("Nie podano nazwy kategorii.".into());
            }
            let (found, suggestions) = helix.find_category(&wanted).await.map_err(e)?;
            let Some(category) = found else {
                return Ok(Outcome { result: json!({ "ok": false, "error": "Nie znaleziono kategorii", "suggestions": suggestions }), action: None });
            };
            helix.modify_channel(&own.id, Some(&category.id), None).await.map_err(e)?;
            ok(json!({ "ok": true, "category": category.name }), Some(format!("🎮 Kategoria → {}", category.name)))
        }
        "set_title" => {
            let title = s("title");
            if title.is_empty() {
                return Err("Tytuł nie może być pusty.".into());
            }
            helix.modify_channel(&own.id, None, Some(&title)).await.map_err(e)?;
            ok(json!({ "ok": true, "title": title }), Some("✏️ Nowy tytuł streamu".into()))
        }
        "timeout_user" | "ban_user" | "unban_user" => {
            let user = resolve_user(helix, ctx, &s("user")).await?;
            let reason = args["reason"].as_str().filter(|r| !r.is_empty());
            match name {
                "timeout_user" => {
                    let secs = args["seconds"].as_u64().unwrap_or(600).clamp(1, 1_209_600) as u32;
                    helix.ban_user(&chat.id, &user.id, Some(secs), reason).await.map_err(e)?;
                    ok(json!({ "ok": true, "user": user.name, "seconds": secs }), Some(format!("⏱ Timeout: {} ({})", user.name, human_duration(secs as u64))))
                }
                "ban_user" => {
                    helix.ban_user(&chat.id, &user.id, None, reason).await.map_err(e)?;
                    ok(json!({ "ok": true, "user": user.name }), Some(format!("🚫 Ban: {}", user.name)))
                }
                _ => {
                    helix.unban_user(&chat.id, &user.id).await.map_err(e)?;
                    ok(json!({ "ok": true, "user": user.name }), Some(format!("✅ Unban: {}", user.name)))
                }
            }
        }
        "add_moderator" | "remove_moderator" | "add_vip" | "remove_vip" => {
            let user = resolve_user(helix, ctx, &s("user")).await?;
            let label = match name {
                "add_moderator" => {
                    helix.add_moderator(&own.id, &user.id).await.map_err(e)?;
                    format!("🛡 Moderator: {}", user.name)
                }
                "remove_moderator" => {
                    helix.remove_moderator(&own.id, &user.id).await.map_err(e)?;
                    format!("🛡 Odebrano moda: {}", user.name)
                }
                "add_vip" => {
                    helix.add_vip(&own.id, &user.id).await.map_err(e)?;
                    format!("💎 VIP: {}", user.name)
                }
                _ => {
                    helix.remove_vip(&own.id, &user.id).await.map_err(e)?;
                    format!("💎 Odebrano VIP: {}", user.name)
                }
            };
            ok(json!({ "ok": true, "user": user.name }), Some(label))
        }
        "warn_user" => {
            let user = resolve_user(helix, ctx, &s("user")).await?;
            let reason = Some(s("reason")).filter(|r| !r.is_empty()).unwrap_or_else(|| "Przestrzegaj zasad czatu.".into());
            helix.warn_user(&chat.id, &user.id, &reason).await.map_err(e)?;
            ok(json!({ "ok": true, "user": user.name }), Some(format!("⚠ Ostrzeżenie: {}", user.name)))
        }
        "set_shield_mode" => {
            let enabled = args["enabled"].as_bool().unwrap_or(true);
            helix.set_shield_mode(&chat.id, enabled).await.map_err(e)?;
            ok(json!({ "ok": true }), Some(format!("🛡 Tryb tarczy: {}", if enabled { "wł." } else { "wył." })))
        }
        "set_nick_color" => {
            let color = chat_color(&s("color")).ok_or("Nieznany kolor nicku.")?;
            helix.set_chat_color(&color).await.map_err(e)?;
            ok(json!({ "ok": true, "color": color }), Some(format!("🖍 Kolor nicku → {color}")))
        }
        "create_prediction" => {
            let title = s("question");
            let outcomes: Vec<String> = args["outcomes"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            if title.is_empty() || outcomes.len() < 2 {
                return Err("Predykcja potrzebuje pytania i co najmniej dwóch odpowiedzi.".into());
            }
            let secs = args["seconds"].as_u64().unwrap_or(120) as u32;
            helix.create_prediction(&own.id, &title, &outcomes, secs).await.map_err(e)?;
            ok(json!({ "ok": true }), Some(format!("🔮 Predykcja: {title}")))
        }
        "end_prediction" => {
            let how = match s("action").as_str() {
                "lock" => PredictionEnd::Lock,
                "cancel" => PredictionEnd::Cancel,
                _ => PredictionEnd::Resolve,
            };
            let winner = s("winner");
            let (title, won) = helix.end_prediction(&own.id, how, Some(winner.as_str())).await.map_err(e)?;
            let label = match (how, &won) {
                (PredictionEnd::Lock, _) => "🔒 Predykcja zablokowana".to_owned(),
                (PredictionEnd::Cancel, _) => "↩ Predykcja anulowana (punkty zwrócone)".to_owned(),
                (_, Some(w)) => format!("🏆 Wygrywa: {w}"),
                _ => "🔮 Predykcja zakończona".to_owned(),
            };
            ok(json!({ "ok": true, "prediction": title, "winner": won }), Some(label))
        }
        "set_chat_mode" => {
            let mode = ChatMode::parse(&s("mode")).ok_or("Nieznany tryb czatu.")?;
            let enabled = args["enabled"].as_bool().unwrap_or(true);
            let seconds = args["seconds"].as_u64().map(|v| v as u32);
            helix.set_chat_mode(&chat.id, mode, enabled, seconds).await.map_err(e)?;
            let state = if enabled { "wł." } else { "wył." };
            ok(json!({ "ok": true }), Some(format!("💬 {}: {state}", mode.label())))
        }
        "clear_chat" => {
            helix.clear_chat(&chat.id).await.map_err(e)?;
            ok(json!({ "ok": true }), Some("🧹 Czat wyczyszczony".into()))
        }
        "send_chat_message" => {
            let text = s("text");
            if text.is_empty() {
                return Err("Pusta wiadomość.".into());
            }
            if args["announcement"].as_bool().unwrap_or(false) {
                helix.announce(&chat.id, &text).await.map_err(e)?;
                ok(json!({ "ok": true }), Some("📣 Ogłoszenie wysłane".into()))
            } else {
                helix.send_chat_message(&chat.id, &text).await.map_err(e)?;
                ok(json!({ "ok": true }), Some("💬 Wiadomość wysłana".into()))
            }
        }
        "shoutout" => {
            let target = resolve_channel(helix, ctx, &s("channel")).await?;
            helix.shoutout(&own.id, &target.id).await.map_err(e)?;
            ok(json!({ "ok": true, "channel": target.name }), Some(format!("📢 Shoutout: {}", target.name)))
        }
        "start_raid" => {
            let target = resolve_channel(helix, ctx, &s("channel")).await?;
            helix.start_raid(&own.id, &target.id).await.map_err(e)?;
            ok(json!({ "ok": true, "channel": target.name }), Some(format!("🚀 Raid → {}", target.name)))
        }
        "cancel_raid" => {
            helix.cancel_raid(&own.id).await.map_err(e)?;
            ok(json!({ "ok": true }), Some("↩ Raid anulowany".into()))
        }
        "create_clip" => {
            let url = helix.create_clip(&own.id).await.map_err(e)?;
            Outcome {
                result: json!({ "ok": true, "url": url }),
                action: Some(Action { text: "🎬 Klip utworzony".into(), ok: true, url: Some(url) }),
            }
        }
        "create_marker" => {
            let description = args["description"].as_str();
            helix.create_marker(&own.id, description).await.map_err(e)?;
            ok(json!({ "ok": true }), Some("📍 Marker dodany".into()))
        }
        "run_ad" => {
            let secs = args["seconds"].as_u64().unwrap_or(60) as u32;
            let (length, retry) = helix.run_commercial(&own.id, secs).await.map_err(e)?;
            ok(json!({ "ok": true, "length": length, "next_possible_in_seconds": retry }), Some(format!("📺 Reklama {length} s")))
        }
        "create_poll" => {
            let title = s("question");
            let choices: Vec<String> = args["choices"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            if title.is_empty() || choices.len() < 2 {
                return Err("Ankieta potrzebuje pytania i co najmniej dwóch odpowiedzi.".into());
            }
            let secs = args["seconds"].as_u64().unwrap_or(120) as u32;
            helix.create_poll(&own.id, &title, &choices, secs).await.map_err(e)?;
            ok(json!({ "ok": true }), Some(format!("📊 Ankieta: {title}")))
        }
        "end_poll" => match helix.end_poll(&own.id).await.map_err(e)? {
            Some(poll) => {
                let label = match poll["winner"].as_str() {
                    Some(w) => format!("⏹ Ankieta zakończona — wygrywa: {w}"),
                    None => "⏹ Ankieta zakończona".into(),
                };
                ok(json!({ "ok": true, "poll": poll }), Some(label))
            }
            None => return Err("Nie ma trwającej ankiety.".into()),
        },
        "poll_results" => match helix.poll_results(&own.id).await.map_err(e)? {
            Some(poll) => ok(json!({ "poll": poll }), None),
            None => ok(json!({ "poll": null, "note": "Na tym kanale nie było jeszcze żadnej ankiety." }), None),
        },
        "prediction_results" => match helix.prediction_results(&own.id).await.map_err(e)? {
            Some(p) => ok(json!({ "prediction": p }), None),
            None => ok(json!({ "prediction": null, "note": "Na tym kanale nie było jeszcze żadnej predykcji." }), None),
        },
        other => return Err(format!("Nieznane narzędzie: {other}")),
    })
}

fn ok(result: Value, action: Option<String>) -> Outcome {
    Outcome { result, action: action.map(|text| Action { text, ok: true, url: None }) }
}

fn fail(message: &str) -> Outcome {
    Outcome {
        result: json!({ "ok": false, "error": message }),
        action: Some(Action { text: format!("⚠ {message}"), ok: false, url: None }),
    }
}

fn is_destructive(name: &str) -> bool {
    // Moderator dostaje pełnię władzy na czacie — przekręcony nick nie może jej dostać.
    matches!(name, "ban_user" | "clear_chat" | "start_raid" | "run_ad" | "add_moderator" | "obs_streaming")
}

fn describe_pending(name: &str, args: &Value) -> String {
    let s = |k: &str| args[k].as_str().unwrap_or("?").to_owned();
    match name {
        "ban_user" => format!("ban dla {}", s("user")),
        "clear_chat" => "wyczyszczenie czatu".into(),
        "start_raid" => format!("raid na {}", s("channel")),
        "run_ad" => format!("reklama {} s", args["seconds"].as_u64().unwrap_or(60)),
        "add_moderator" => format!("moderator dla {}", s("user")),
        "obs_streaming" if s("action") == "stop" => "zakończenie streama w OBS".to_owned(),
        "obs_streaming" => "start streama w OBS".to_owned(),
        other => other.to_owned(),
    }
}

/// Kolor panelu: polska nazwa (dowolna forma) albo #RRGGBB → (hex, etykieta).
fn panel_color(input: &str) -> Option<(String, String)> {
    if let Some(hex) = crate::config::parse_accent(input) {
        return Some((hex.clone(), hex));
    }
    let n = fold(input);
    const NAMES: &[(&[&str], &str, &str)] = &[
        (&["fiolet", "liliow", "purpur", "violet", "lawend"], "#8b6cf0", "fiolet"),
        (&["roz", "pink", "magent"], "#f0619e", "róż"),
        (&["czerw", "red", "karmaz"], "#f0505a", "czerwień"),
        (&["pomarancz", "orange"], "#f08a3c", "pomarańcz"),
        (&["zlot", "zolt", "gold", "yellow", "bursztyn"], "#e8b84a", "złoto"),
        (&["zielon", "green", "szmaragd", "limonk"], "#3fcf8e", "zieleń"),
        (&["turkus", "cyjan", "cyan", "morsk", "akwa"], "#33c4d6", "turkus"),
        (&["niebiesk", "blue", "granat", "blekit"], "#4f8cf0", "niebieski"),
        (&["srebr", "bial", "szar", "silver", "white", "grey", "gray"], "#c9cfdc", "srebro"),
    ];
    NAMES.iter().find(|(keys, ..)| keys.iter().any(|k| n.contains(k))).map(|(_, hex, label)| ((*hex).into(), (*label).into()))
}

/// Kolor nicku Twitch: nazwa z listy Twitcha, polska nazwa albo #RRGGBB.
fn chat_color(input: &str) -> Option<String> {
    let raw = input.trim().to_lowercase().replace([' ', '-'], "_");
    if CHAT_COLORS.contains(&raw.as_str()) {
        return Some(raw);
    }
    if let Some(hex) = crate::config::parse_accent(input) {
        return Some(hex);
    }
    let n = fold(input);
    const PL: &[(&str, &str)] = &[
        ("fiolet", "blue_violet"), ("roz", "hot_pink"), ("czerw", "red"), ("pomarancz", "orange_red"), ("zlot", "golden_rod"),
        ("zolt", "golden_rod"), ("koral", "coral"), ("czekolad", "chocolate"), ("brazow", "chocolate"), ("ceglast", "firebrick"),
        ("morsk", "sea_green"), ("limonk", "yellow_green"), ("zielon", "green"), ("blekit", "dodger_blue"), ("niebiesk", "blue"),
    ];
    PL.iter().find(|(k, _)| n.contains(k)).map(|(_, v)| (*v).to_owned())
}

/// Małe litery bez polskich znaków („Różowy” → „rozowy”).
pub(crate) fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'ą' => 'a', 'ć' => 'c', 'ę' => 'e', 'ł' => 'l', 'ń' => 'n', 'ó' => 'o', 'ś' => 's', 'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

/// Losowość bez dodatkowych bibliotek (czas systemowy + mieszanie).
fn random_index(len: usize) -> usize {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let mut x = nanos ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    (x % len as u64) as usize
}

fn demo_label(name: &str, args: &Value) -> String {
    let s = |k: &str| args[k].as_str().unwrap_or_default().to_owned();
    match name {
        "set_chat_mode" => format!("💬 {}: {}", ChatMode::parse(&s("mode")).map_or("Tryb czatu", ChatMode::label), if args["enabled"].as_bool().unwrap_or(true) { "wł." } else { "wył." }),
        "clear_chat" => "🧹 Czat wyczyszczony".into(),
        "send_chat_message" => "💬 Wiadomość wysłana".into(),
        "shoutout" => format!("📢 Shoutout: {}", s("channel")),
        "start_raid" => format!("🚀 Raid → {}", s("channel")),
        "create_clip" => "🎬 Klip utworzony".into(),
        "create_marker" => "📍 Marker dodany".into(),
        "run_ad" => format!("📺 Reklama {} s", args["seconds"].as_u64().unwrap_or(60)),
        "create_poll" => format!("📊 Ankieta: {}", s("question")),
        "end_poll" => "⏹ Ankieta zakończona".into(),
        "set_shield_mode" => format!("🛡 Tryb tarczy: {}", if args["enabled"].as_bool().unwrap_or(true) { "wł." } else { "wył." }),
        "set_nick_color" => format!("🖍 Kolor nicku → {}", chat_color(&s("color")).unwrap_or_else(|| s("color"))),
        "create_prediction" => format!("🔮 Predykcja: {}", s("question")),
        "end_prediction" => match s("action").as_str() {
            "lock" => "🔒 Predykcja zablokowana".into(),
            "cancel" => "↩ Predykcja anulowana".into(),
            _ => format!("🏆 Wygrywa: {}", s("winner")),
        },
        other => format!("✓ {other}"),
    }
}

fn recent_json(ctx: &Context, args: &Value) -> Vec<Value> {
    let count = args["count"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
    ctx.recent.iter().rev().take(count).rev().map(|(n, t)| json!({ "user": n, "text": t })).collect()
}

/// Dopasowanie nicku z mowy do osoby z czatu (rozpoznawanie mowy przekręca nicki).
fn resolve_known(ctx: &Context, spoken: &str) -> Option<Chatter> {
    let wanted = normalize(spoken);
    if wanted.is_empty() {
        return None;
    }
    if let Some(c) = ctx.chatters.iter().find(|c| normalize(&c.name) == wanted || normalize(&c.login) == wanted) {
        return Some(c.clone());
    }
    // Najbliższy nick w odległości edycyjnej ≤ 2 (dla nicków ≥ 4 znaków), tylko jednoznaczny.
    if wanted.chars().count() < 4 {
        return None;
    }
    let mut scored: Vec<(usize, &Chatter)> = ctx.chatters.iter().map(|c| (levenshtein(&wanted, &normalize(&c.login)), c)).collect();
    scored.sort_by_key(|(d, _)| *d);
    match scored.as_slice() {
        [(d, c), rest @ ..] if *d <= 2 && rest.first().is_none_or(|(d2, _)| d2 > d) => Some((*c).clone()),
        _ => None,
    }
}

async fn resolve_user(helix: &Helix, ctx: &Context, spoken: &str) -> Result<Chatter, String> {
    if let Some(c) = resolve_known(ctx, spoken) {
        return Ok(c);
    }
    let login: String = spoken.trim().trim_start_matches('@').chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    if login.is_empty() {
        return Err(format!("Nie wiem, kim jest „{spoken}”."));
    }
    match helix.user_by_login(&login).await.map_err(|e| e.to_string())? {
        Some(u) => Ok(Chatter { name: u.display_name, login: u.login, id: u.id }),
        None => Err(format!("Nie znalazłem użytkownika „{spoken}” — powtórz nick albo przeliteruj.")),
    }
}

async fn resolve_channel(helix: &Helix, ctx: &Context, spoken: &str) -> Result<Chatter, String> {
    resolve_user(helix, ctx, spoken).await
}

/// Najbliższa nazwa z listy (sceny, źródła): dokładna → zawiera → literówki.
pub(crate) fn best_match(spoken: &str, names: &[String]) -> Option<String> {
    let norm = |s: &str| fold(s).chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let wanted = norm(spoken);
    if wanted.is_empty() {
        return None;
    }
    if let Some(n) = names.iter().find(|n| norm(n) == wanted) {
        return Some(n.clone());
    }
    let containing: Vec<&String> = names.iter().filter(|n| norm(n).contains(&wanted) || wanted.contains(&norm(n))).collect();
    if containing.len() == 1 {
        return Some(containing[0].clone());
    }
    let mut scored: Vec<(usize, &String)> = names.iter().map(|n| (levenshtein(&wanted, &norm(n)), n)).collect();
    scored.sort_by_key(|(d, _)| *d);
    match scored.as_slice() {
        [(d, n), rest @ ..] if *d <= 2.max(wanted.len() / 4) && rest.first().is_none_or(|(d2, _)| d2 > d) => Some((*n).clone()),
        _ => None,
    }
}

/// Polecenia OBS (obs-websocket).
async fn run_obs(obs: &crate::obs::ObsHandle, name: &str, args: &Value) -> Result<Outcome, String> {
    let s = |key: &str| args[key].as_str().unwrap_or_default().trim().to_owned();
    match name {
        "obs_switch_scene" => {
            let list = obs.request("GetSceneList", json!({})).await?;
            let scenes: Vec<String> = list["scenes"].as_array().map(|a| a.iter().filter_map(|x| x["sceneName"].as_str().map(str::to_owned)).collect()).unwrap_or_default();
            let scene = best_match(&s("scene"), &scenes).ok_or_else(|| format!("Nie ma sceny „{}”. Sceny: {}.", s("scene"), scenes.join(", ")))?;
            obs.request("SetCurrentProgramScene", json!({ "sceneName": scene })).await?;
            Ok(ok(json!({ "ok": true, "scene": scene }), Some(format!("🎬 Scena → {scene}"))))
        }
        "obs_set_mute" => {
            let inputs = obs.request("GetInputList", json!({})).await?;
            let list = inputs["inputs"].as_array().cloned().unwrap_or_default();
            let names: Vec<String> = list.iter().filter_map(|i| i["inputName"].as_str().map(str::to_owned)).collect();
            let wanted = s("source");
            let micish = ["", "mikrofon", "mic", "mikrofonu", "mikrofonik", "glos", "głos"];
            let input = if micish.contains(&fold(&wanted).as_str()) {
                // Mikrofon: pierwsze wejście audio (WASAPI / Pulse / CoreAudio input capture).
                list.iter()
                    .find(|i| i["inputKind"].as_str().is_some_and(|k| k.contains("input_capture")))
                    .and_then(|i| i["inputName"].as_str().map(str::to_owned))
                    .or_else(|| best_match("mic", &names))
            } else {
                best_match(&wanted, &names)
            }
            .ok_or_else(|| format!("Nie znalazłem źródła dźwięku „{wanted}”. Źródła: {}.", names.join(", ")))?;
            let muted = match args["muted"].as_bool() {
                Some(m) => {
                    obs.request("SetInputMute", json!({ "inputName": input, "inputMuted": m })).await?;
                    m
                }
                None => obs.request("ToggleInputMute", json!({ "inputName": input })).await?["inputMuted"].as_bool().unwrap_or(true),
            };
            Ok(ok(json!({ "ok": true, "source": input, "muted": muted }), Some(format!("{} {input}", if muted { "🔇 Wyciszono" } else { "🔊 Włączono" }))))
        }
        "obs_set_source_visible" => {
            let current = obs.request("GetCurrentProgramScene", json!({})).await?;
            let scene = current["currentProgramSceneName"].as_str().or_else(|| current["sceneName"].as_str()).unwrap_or_default().to_owned();
            let items = obs.request("GetSceneItemList", json!({ "sceneName": scene })).await?;
            let list = items["sceneItems"].as_array().cloned().unwrap_or_default();
            let names: Vec<String> = list.iter().filter_map(|i| i["sourceName"].as_str().map(str::to_owned)).collect();
            let wanted = s("source");
            let alias = match fold(&wanted).as_str() {
                "kamerka" | "kamera" | "kamere" | "kamerke" | "facecam" | "cam" => "kamer",
                other => other,
            }
            .to_owned();
            let source = best_match(&wanted, &names)
                .or_else(|| names.iter().find(|n| fold(n).contains(&alias) || (alias == "kamer" && (fold(n).contains("cam") || fold(n).contains("video")))).cloned())
                .ok_or_else(|| format!("Na scenie „{scene}” nie ma źródła „{wanted}”. Źródła: {}.", names.join(", ")))?;
            let item = list.iter().find(|i| i["sourceName"].as_str() == Some(&source)).and_then(|i| i["sceneItemId"].as_i64()).unwrap_or_default();
            let visible = args["visible"].as_bool().unwrap_or(true);
            obs.request("SetSceneItemEnabled", json!({ "sceneName": scene, "sceneItemId": item, "sceneItemEnabled": visible })).await?;
            Ok(ok(json!({ "ok": true, "source": source, "visible": visible }), Some(format!("{} {source}", if visible { "👁 Pokazano" } else { "🙈 Schowano" }))))
        }
        "obs_recording" => {
            let (request, label) = match s("action").as_str() {
                "stop" => ("StopRecord", "⏹ Nagrywanie zatrzymane"),
                "pause" => ("PauseRecord", "⏸ Nagrywanie wstrzymane"),
                "resume" => ("ResumeRecord", "⏺ Nagrywanie wznowione"),
                _ => ("StartRecord", "⏺ Nagrywanie rozpoczęte"),
            };
            obs.request(request, json!({})).await?;
            Ok(ok(json!({ "ok": true }), Some(label.into())))
        }
        "obs_streaming" => {
            let stop = s("action") == "stop";
            obs.request(if stop { "StopStream" } else { "StartStream" }, json!({})).await?;
            Ok(ok(json!({ "ok": true }), Some(if stop { "⏹ Stream zakończony" } else { "🔴 Stream wystartował" }.into())))
        }
        "obs_save_replay" => {
            obs.request("SaveReplayBuffer", json!({})).await?;
            Ok(ok(json!({ "ok": true }), Some("💾 Powtórka zapisana".into())))
        }
        _ => {
            let st = obs.status();
            Ok(ok(json!({ "connected": st.connected, "scenes": st.scenes, "current_scene": st.current_scene, "streaming": st.streaming, "recording": st.recording }), None))
        }
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

pub fn human_duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3_600 => format!("{} min", s / 60),
        s if s < 86_400 => {
            let m = (s % 3_600) / 60;
            if m == 0 { format!("{} h", s / 3_600) } else { format!("{} h {m} min", s / 3_600) }
        }
        s => format!("{} dni", s / 86_400),
    }
}

/// Deklaracje funkcji dla Gemini (podzbiór schematu OpenAPI).
pub fn declarations() -> Value {
    let user = json!({ "type": "string", "description": "Nick użytkownika Twitch — wybierz najbliższy z listy osób na czacie" });
    json!([
        { "name": "get_stream_info", "description": "Informacje o Twoim streamie: czy trwa, tytuł, kategoria, liczba widzów, czas trwania, liczba obserwujących." },
        { "name": "get_recent_chat", "description": "Ostatnie wiadomości z czatu (np. żeby streszczać, o czym piszą).",
          "parameters": { "type": "object", "properties": { "count": { "type": "integer", "description": "Ile wiadomości (1–50), domyślnie 20" } } } },
        { "name": "set_category", "description": "Zmienia kategorię (grę) Twojego streamu.",
          "parameters": { "type": "object", "properties": { "name": { "type": "string", "description": "Pełna oficjalna nazwa kategorii na Twitchu, np. 'Grand Theft Auto IV', 'Just Chatting', 'League of Legends'" } }, "required": ["name"] } },
        { "name": "set_title", "description": "Zmienia tytuł Twojego streamu (max 140 znaków).",
          "parameters": { "type": "object", "properties": { "title": { "type": "string" } }, "required": ["title"] } },
        { "name": "timeout_user", "description": "Daje użytkownikowi timeout (wyciszenie) na czacie.",
          "parameters": { "type": "object", "properties": { "user": user, "seconds": { "type": "integer", "description": "Czas w sekundach, domyślnie 600" }, "reason": { "type": "string" } }, "required": ["user"] } },
        { "name": "ban_user", "description": "Banuje użytkownika na czacie (wymaga potwierdzenia).",
          "parameters": { "type": "object", "properties": { "user": user, "reason": { "type": "string" } }, "required": ["user"] } },
        { "name": "unban_user", "description": "Zdejmuje ban lub timeout.",
          "parameters": { "type": "object", "properties": { "user": user }, "required": ["user"] } },
        { "name": "set_chat_mode", "description": "Włącza lub wyłącza tryb czatu.",
          "parameters": { "type": "object", "properties": {
              "mode": { "type": "string", "enum": ["slow", "emote_only", "followers_only", "subscribers_only"] },
              "enabled": { "type": "boolean" },
              "seconds": { "type": "integer", "description": "Slow mode: odstęp 3–120 s; followers_only: minimalny czas obserwowania w sekundach" } },
            "required": ["mode", "enabled"] } },
        { "name": "clear_chat", "description": "Czyści cały czat (wymaga potwierdzenia)." },
        { "name": "send_chat_message", "description": "Wysyła wiadomość na czat w Twoim imieniu.",
          "parameters": { "type": "object", "properties": { "text": { "type": "string" }, "announcement": { "type": "boolean", "description": "true = wyróżnione ogłoszenie" } }, "required": ["text"] } },
        { "name": "shoutout", "description": "Robi shoutout innemu streamerowi (wymaga trwającego streamu).",
          "parameters": { "type": "object", "properties": { "channel": { "type": "string", "description": "Login kanału" } }, "required": ["channel"] } },
        { "name": "start_raid", "description": "Rozpoczyna raid na inny kanał (wymaga potwierdzenia).",
          "parameters": { "type": "object", "properties": { "channel": { "type": "string", "description": "Login kanału" } }, "required": ["channel"] } },
        { "name": "cancel_raid", "description": "Anuluje rozpoczęty raid." },
        { "name": "create_clip", "description": "Tworzy klip z ostatnich sekund streamu." },
        { "name": "create_marker", "description": "Dodaje marker w nagraniu streamu (do późniejszego montażu).",
          "parameters": { "type": "object", "properties": { "description": { "type": "string" } } } },
        { "name": "run_ad", "description": "Puszcza reklamę (wymaga potwierdzenia).",
          "parameters": { "type": "object", "properties": { "seconds": { "type": "integer", "description": "30, 60, 90, 120, 150 lub 180" } } } },
        { "name": "create_poll", "description": "Tworzy ankietę na Twoim kanale.",
          "parameters": { "type": "object", "properties": {
              "question": { "type": "string", "description": "Pytanie, max 60 znaków" },
              "choices": { "type": "array", "items": { "type": "string" }, "description": "2–5 odpowiedzi, każda max 25 znaków" },
              "seconds": { "type": "integer", "description": "Czas trwania 15–1800 s, domyślnie 120" } },
            "required": ["question", "choices"] } },
        { "name": "end_poll", "description": "Kończy trwającą ankietę i zwraca jej wyniki." },
        { "name": "poll_results", "description": "Wyniki ostatniej ankiety na kanale — także już zakończonej (głosy, procenty, zwycięzca). Użyj, gdy pytają o wyniki ankiety." },
        { "name": "prediction_results", "description": "Wyniki ostatniej predykcji — także zakończonej (ile osób i punktów na każdą odpowiedź, zwycięzca)." },
        { "name": "add_moderator", "description": "Daje użytkownikowi moderatora na Twoim kanale (wymaga potwierdzenia).",
          "parameters": { "type": "object", "properties": { "user": user }, "required": ["user"] } },
        { "name": "remove_moderator", "description": "Odbiera moderatora.",
          "parameters": { "type": "object", "properties": { "user": user }, "required": ["user"] } },
        { "name": "add_vip", "description": "Daje użytkownikowi VIP-a na Twoim kanale.",
          "parameters": { "type": "object", "properties": { "user": user }, "required": ["user"] } },
        { "name": "remove_vip", "description": "Odbiera VIP-a.",
          "parameters": { "type": "object", "properties": { "user": user }, "required": ["user"] } },
        { "name": "warn_user", "description": "Ostrzega użytkownika (musi potwierdzić ostrzeżenie, zanim znów napisze). Łagodniejsze niż timeout.",
          "parameters": { "type": "object", "properties": { "user": user, "reason": { "type": "string", "description": "Powód, widoczny dla użytkownika" } }, "required": ["user"] } },
        { "name": "set_shield_mode", "description": "Włącza/wyłącza tryb tarczy (ochrona przed nalotem botów i hejtu).",
          "parameters": { "type": "object", "properties": { "enabled": { "type": "boolean" } }, "required": ["enabled"] } },
        { "name": "set_nick_color", "description": "Zmienia kolor Twojego nicku na czacie Twitcha.",
          "parameters": { "type": "object", "properties": { "color": { "type": "string", "description": "Jeden z: blue, blue_violet, cadet_blue, chocolate, coral, dodger_blue, firebrick, golden_rod, green, hot_pink, orange_red, red, sea_green, spring_green, yellow_green; #RRGGBB tylko z Prime/Turbo" } }, "required": ["color"] } },
        { "name": "create_prediction", "description": "Tworzy predykcję (zakład punktami kanału).",
          "parameters": { "type": "object", "properties": {
              "question": { "type": "string", "description": "Pytanie, max 45 znaków" },
              "outcomes": { "type": "array", "items": { "type": "string" }, "description": "2–10 odpowiedzi, każda max 25 znaków" },
              "seconds": { "type": "integer", "description": "Czas na obstawianie 30–1800 s, domyślnie 120" } },
            "required": ["question", "outcomes"] } },
        { "name": "end_prediction", "description": "Kończy trwającą predykcję: rozstrzyga (resolve, podaj zwycięzcę), blokuje obstawianie (lock) albo anuluje i zwraca punkty (cancel).",
          "parameters": { "type": "object", "properties": {
              "action": { "type": "string", "enum": ["resolve", "lock", "cancel"] },
              "winner": { "type": "string", "description": "Zwycięska odpowiedź (przy resolve)" } },
            "required": ["action"] } },
        { "name": "set_panel_color", "description": "Zmienia kolor motywu panelu LiMBOCHAT w OBS.",
          "parameters": { "type": "object", "properties": { "color": { "type": "string", "description": "fiolet, róż, czerwień, pomarańcz, złoto, zieleń, turkus, niebieski, srebro albo #RRGGBB" } }, "required": ["color"] } },
        { "name": "open_channel", "description": "Otwiera czat kanału w panelu (albo przełącza na już otwarty).",
          "parameters": { "type": "object", "properties": { "channel": { "type": "string", "description": "Login kanału" } }, "required": ["channel"] } },
        { "name": "set_reminder", "description": "Ustawia przypomnienie — LimboAI powie je na głos po czasie.",
          "parameters": { "type": "object", "properties": { "seconds": { "type": "integer", "description": "Za ile sekund (max 6 h)" }, "text": { "type": "string", "description": "O czym przypomnieć" } }, "required": ["seconds", "text"] } },
        { "name": "cancel_reminders", "description": "Anuluje wszystkie przypomnienia." },
        { "name": "check_update", "description": "Sprawdza od razu, czy jest nowa wersja LiMBOCHAT (pobiera ją, jeśli jest). Gdy jest — podaj numer i zapytaj, czy zainstalować." },
        { "name": "install_update", "description": "Instaluje pobraną nową wersję LiMBOCHAT (bez restartu OBS, zaraz po tej odpowiedzi). Tylko gdy streamer o to prosi." },
        { "name": "pick_random_chatter", "description": "Losuje osobę spośród ostatnio piszących na czacie (np. do giveawaya)." },
        { "name": "obs_switch_scene", "description": "Przełącza scenę w OBS (np. Start, Gra, Przerwa, Koniec).",
          "parameters": { "type": "object", "properties": { "scene": { "type": "string", "description": "Nazwa sceny z listy scen OBS" } }, "required": ["scene"] } },
        { "name": "obs_set_mute", "description": "Wycisza albo włącza źródło dźwięku w OBS. Bez nazwy = mikrofon. Bez muted = przełącza.",
          "parameters": { "type": "object", "properties": { "source": { "type": "string", "description": "Nazwa źródła dźwięku, np. Mikrofon, Dźwięk pulpitu, Muzyka" }, "muted": { "type": "boolean" } } } },
        { "name": "obs_set_source_visible", "description": "Pokazuje albo chowa źródło na aktualnej scenie OBS (np. kamerkę).",
          "parameters": { "type": "object", "properties": { "source": { "type": "string", "description": "Nazwa źródła, np. Kamerka" }, "visible": { "type": "boolean" } }, "required": ["source", "visible"] } },
        { "name": "obs_recording", "description": "Nagrywanie w OBS: start, stop, pause, resume.",
          "parameters": { "type": "object", "properties": { "action": { "type": "string", "enum": ["start", "stop", "pause", "resume"] } }, "required": ["action"] } },
        { "name": "obs_streaming", "description": "Rozpoczyna albo kończy stream w OBS (wymaga potwierdzenia).",
          "parameters": { "type": "object", "properties": { "action": { "type": "string", "enum": ["start", "stop"] } }, "required": ["action"] } },
        { "name": "obs_save_replay", "description": "Zapisuje powtórkę z bufora powtórek OBS (ostatnie sekundy)." },
        { "name": "mark_moment", "description": "Zapisuje czas streama i fragment czatu jako ciekawy moment do montażu. Użyj na 'zapamiętaj ten moment', 'zapisz znacznik', 'ale była akcja'.",
          "parameters":{"type":"object","properties":{"label":{"type":"string","description":"Krótki opis momentu"}}} },
        { "name": "obs_status", "description": "Stan OBS: sceny, aktualna scena, czy trwa stream i nagrywanie." },
        { "name": "remember_viewer", "description": "Zapamiętuje trwały fakt o widzu (np. lubi koty, gra w CS-a).",
          "parameters": { "type": "object", "properties": { "user": user, "fact": { "type": "string", "description": "Krótki fakt w 3. osobie, bez nicku, np. 'lubi koty'" } }, "required": ["user", "fact"] } },
        { "name": "forget_viewer", "description": "Zapomina fakt o widzu (albo wszystko, gdy bez faktu).",
          "parameters": { "type": "object", "properties": { "user": user, "fact": { "type": "string" } }, "required": ["user"] } },
        { "name": "viewer_info", "description": "Co LimboAI pamięta o widzu: notatki, od kiedy ogląda, ile streamów, wsparcie.",
          "parameters": { "type": "object", "properties": { "user": user }, "required": ["user"] } },
        { "name": "confirm_pending", "description": "Potwierdza (true) albo anuluje (false) akcję czekającą na potwierdzenie.",
          "parameters": { "type": "object", "properties": { "confirmed": { "type": "boolean" } }, "required": ["confirmed"] } }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            me: Some(("streamer".into(), "1".into())),
            demo: true,
            chatters: vec![
                Chatter { name: "ModZiomek".into(), login: "modziomek".into(), id: "10".into() },
                Chatter { name: "NocnyMarek".into(), login: "nocnymarek".into(), id: "11".into() },
                Chatter { name: "kasia_gra".into(), login: "kasia_gra".into(), id: "12".into() },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn spoken_names_map_to_chatters() {
        let c = ctx();
        assert_eq!(resolve_known(&c, "mod ziomek").unwrap().login, "modziomek");
        assert_eq!(resolve_known(&c, "Kasia gra").unwrap().login, "kasia_gra");
        assert_eq!(resolve_known(&c, "nocny marke").unwrap().login, "nocnymarek", "literówka z rozpoznawania mowy");
        assert!(resolve_known(&c, "ktoś zupełnie inny").is_none());
    }

    #[tokio::test]
    async fn destructive_actions_require_a_separate_confirmation() {
        let mut ex = Executor::new(None, Local::default());
        let c = ctx();
        let first = ex.run(&c, "ban_user", &json!({ "user": "ModZiomek" })).await;
        assert_eq!(first.result["status"], "needs_confirmation");
        assert_eq!(ex.pending_label().as_deref(), Some("ban dla ModZiomek"));

        let confirmed = ex.run(&c, "confirm_pending", &json!({ "confirmed": true })).await;
        assert_eq!(confirmed.result["ok"], true);
        assert!(confirmed.action.unwrap().text.contains("Ban"));

        let again = ex.run(&c, "confirm_pending", &json!({ "confirmed": true })).await;
        assert_eq!(again.result["ok"], false, "potwierdzenie działa tylko raz");
    }

    #[tokio::test]
    async fn cancelling_pending_action() {
        let mut ex = Executor::new(None, Local::default());
        let c = ctx();
        ex.run(&c, "clear_chat", &json!({})).await;
        let out = ex.run(&c, "confirm_pending", &json!({ "confirmed": false })).await;
        assert!(out.action.unwrap().text.contains("Anulowano"));
        assert!(ex.pending_label().is_none());
    }

    #[test]
    fn declarations_are_valid_json_schema_subset() {
        let decls = declarations();
        let names: Vec<&str> = decls.as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"set_category") && names.contains(&"confirm_pending"));
        for d in decls.as_array().unwrap() {
            if let Some(p) = d.get("parameters") {
                assert_eq!(p["type"], "object", "{}", d["name"]);
            }
        }
    }

    #[test]
    fn color_names_in_polish() {
        assert_eq!(panel_color("różowy").unwrap().0, "#f0619e");
        assert_eq!(panel_color("na Zielony").unwrap().1, "zieleń");
        assert_eq!(panel_color("#FF0000").unwrap().0, "#ff0000");
        assert!(panel_color("tęczowy").is_none());
        assert_eq!(chat_color("Hot Pink").as_deref(), Some("hot_pink"));
        assert_eq!(chat_color("czerwony").as_deref(), Some("red"));
        assert_eq!(chat_color("błękitny").as_deref(), Some("dodger_blue"));
    }

    #[tokio::test]
    async fn giving_moderator_needs_confirmation() {
        let mut ex = Executor::new(None, Local::default());
        let out = ex.run(&ctx(), "add_moderator", &json!({ "user": "kasia gra" })).await;
        assert_eq!(out.result["status"], "needs_confirmation");
    }

    #[tokio::test]
    async fn random_chatter_skips_streamer() {
        let mut ex = Executor::new(None, Local::default());
        let mut c = ctx();
        c.chatters.push(Chatter { name: "Streamer".into(), login: "streamer".into(), id: "1".into() });
        for _ in 0..20 {
            let out = ex.run(&c, "pick_random_chatter", &json!({})).await;
            assert_ne!(out.result["winner"], "Streamer");
        }
    }

    #[tokio::test]
    async fn reminders_are_sent_to_jarvis() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ex = Executor::new(None, Local { reminders: Some(tx), ..Default::default() });
        ex.run(&ctx(), "set_reminder", &json!({ "seconds": 600, "text": "napij się wody" })).await;
        match rx.recv().await.unwrap() {
            ReminderCmd::Add { after, text } => assert_eq!((after.as_secs(), text.as_str()), (600, "napij się wody")),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn install_update_only_when_a_version_is_ready() {
        let mut ex = Executor::new(None, Local::default());
        let out = ex.run(&ctx(), "install_update", &json!({})).await;
        assert!(out.action.as_ref().is_some_and(|a| !a.ok), "bez nowej wersji nie ma czego instalować");
        let ready = Context { update: Some(("0.7.3".into(), "- Nowy głos".into())), ..ctx() };
        let out = ex.run(&ready, "install_update", &json!({})).await;
        assert!(out.action.as_ref().is_some_and(|a| !a.ok), "bez panelu (hub) instalacja niemożliwa");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(600), "10 min");
        assert_eq!(human_duration(6_120), "1 h 42 min");
    }
}
