//! Orkiestrator działający w runtime Tokio.
//!
//! Jedna pętla `select!` obsługuje trzy strumienie:
//! * wiadomości z IRC (wszystkie otwarte kanały) → parsowanie → `UiEvent`,
//! * komendy z UI → kanały, logowanie, wysyłanie wiadomości, moderacja,
//! * zdarzenia wewnętrzne z zadań w tle (np. „zalogowano”).
//!
//! Wszystko, co wymaga sieci (7TV, Helix, OAuth), działa w osobnych
//! zadaniach — wolne zapytanie nigdy nie wstrzyma strumienia wiadomości.
//!
//! Czat czytamy anonimowo, a piszemy i moderujemy przez Helix. Dzięki temu
//! zalogowanie, wylogowanie ani odświeżenie tokena nie zrywają połączenia
//! z czatem, a token nigdy nie trafia do połączenia IRC.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    runtime::Handle,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};

use crate::{
    auth::{self, AuthClient, TokenManager, UserInfo},
    emotes::{EmoteScale, EmoteSource, EmoteStore},
    events::{AuthState, ChannelRef, Command, UiEvent, UiSender},
    helix::{Helix, HelixError},
    irc::{self, AnonymousClient, IrcEvent},
    seventv::{ImageFormat, SevenTvClient},
};

/// Twitch wymaga walidacji tokenów OAuth co godzinę.
const VALIDATION_INTERVAL: Duration = Duration::from_secs(3_600);
/// Twitch odrzuca identyczną wiadomość wysłaną ponownie w ciągu 30 s.
const DUPLICATE_WINDOW: Duration = Duration::from_secs(30);
/// Niewidoczny znak, którym klienty 7TV/Chatterino obchodzą filtr duplikatów.
/// Nasz parser usuwa go przy wyświetlaniu.
const DUPLICATE_BYPASS: &str = " \u{E0000}";

pub struct BackendConfig {
    pub emote_scale: EmoteScale,
    pub image_format: ImageFormat,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self { emote_scale: EmoteScale::X1, image_format: ImageFormat::Webp }
    }
}

/// Uchwyty do działającego backendu.
pub struct Backend {
    pub commands: UnboundedSender<Command>,
    /// Magazyn emotek — UI też z niego czyta (picker, podpowiedzi).
    pub emotes: Arc<EmoteStore>,
    /// Klient Helix ze wspólną sesją (wtyczka OBS używa go dla Jarvisa).
    /// Wspólna sesja jest konieczna: refresh token Twitcha jest jednorazowy.
    #[allow(dead_code)]
    pub helix: Arc<Helix>,
}

/// Uruchamia backend na podanym runtime.
pub fn spawn(rt: &Handle, ui: UiSender, config: BackendConfig) -> Backend {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let store = Arc::new(EmoteStore::default());

    let http = build_http_client();
    let client_id = auth::client_id();
    let tokens = Arc::new(TokenManager::new(AuthClient::new(http.clone(), client_id.clone())));
    let helix = Arc::new(Helix::new(http.clone(), client_id, tokens.clone()));

    rt.spawn(run(ui, cmd_rx, store.clone(), config, http, tokens, helix.clone()));
    Backend { commands: cmd_tx, emotes: store, helix }
}

pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("LiMBOCHAT/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .build()
        .expect("konfiguracja klienta HTTP jest statyczna i poprawna")
}

/// Współdzielone usługi — tanie do klonowania (same `Arc`).
#[derive(Clone)]
struct Services {
    ui: UiSender,
    store: Arc<EmoteStore>,
    emote_source: Arc<dyn EmoteSource>,
    tokens: Arc<TokenManager>,
    helix: Arc<Helix>,
    internal: UnboundedSender<Internal>,
}

/// Zdarzenia z zadań w tle, które muszą zmienić stan pętli głównej.
enum Internal {
    LoggedIn,
}

#[derive(Default)]
struct State {
    /// Otwarte kanały: login → ID (znane po ROOMSTATE).
    channels: HashMap<String, Option<String>>,
    login_task: Option<JoinHandle<()>>,
    /// Ostatnio wysłana wiadomość na kanał — do obejścia filtra duplikatów.
    last_sent: HashMap<String, (String, Instant)>,
}

async fn run(
    ui: UiSender,
    mut cmd_rx: UnboundedReceiver<Command>,
    store: Arc<EmoteStore>,
    config: BackendConfig,
    http: reqwest::Client,
    tokens: Arc<TokenManager>,
    helix: Arc<Helix>,
) {
    let (internal_tx, mut internal_rx) = mpsc::unbounded_channel();

    let svc = Services {
        ui,
        store,
        emote_source: Arc::new(SevenTvClient::new(http, config.emote_scale, config.image_format)),
        helix,
        tokens,
        internal: internal_tx,
    };

    tokio::spawn(load_global_emotes(svc.clone()));
    tokio::spawn(restore_session(svc.clone()));
    tokio::spawn(periodic_validation(svc.clone()));

    let (mut incoming, client) = irc::connect_anonymous();
    let mut state = State::default();

    loop {
        tokio::select! {
            Some(server_msg) = incoming.recv() => {
                match irc::translate(server_msg, &svc.store, config.emote_scale) {
                    Some(IrcEvent::Ui(ev)) => svc.ui.send(ev),
                    Some(IrcEvent::RoomState { channel_login, channel_id }) => {
                        on_room_state(&svc, &mut state, channel_login, channel_id);
                    }
                    None => {}
                }
            }
            Some(event) = internal_rx.recv() => match event {
                Internal::LoggedIn => {
                    for (login, id) in &state.channels {
                        if let Some(id) = id {
                            refresh_channel_context(&svc, ChannelRef { login: login.clone(), id: id.clone() });
                        }
                    }
                }
            },
            cmd = cmd_rx.recv() => match cmd {
                None | Some(Command::Shutdown) => break,
                Some(Command::Join(name)) => join_channel(&svc, &client, &mut state, &name),
                Some(Command::Part(name)) => part_channel(&svc, &client, &mut state, &name),
                Some(Command::Login) => start_login(&svc, &mut state),
                Some(Command::CancelLogin) => {
                    if let Some(task) = state.login_task.take() {
                        task.abort();
                    }
                    svc.ui.send(UiEvent::Auth(AuthState::LoggedOut));
                }
                Some(Command::Logout) => {
                    let svc = svc.clone();
                    tokio::spawn(async move {
                        svc.tokens.logout().await;
                        svc.ui.send(UiEvent::Auth(AuthState::LoggedOut));
                        svc.ui.send(UiEvent::notice("Wylogowano. Token został unieważniony."));
                    });
                }
                Some(Command::SendMessage { channel, text }) => send_message(&svc, &mut state, channel, text),
                Some(cmd) => moderate(&svc, cmd),
            },
        }
    }

    tracing::info!("backend zakończył pracę");
}

// ── Kanały ─────────────────────────────────────────────────────────────

fn normalize(name: &str) -> String {
    name.trim().trim_start_matches('#').to_lowercase()
}

fn join_channel(svc: &Services, client: &AnonymousClient, state: &mut State, name: &str) {
    let login = normalize(name);
    if state.channels.contains_key(&login) {
        return;
    }
    match client.join(login.clone()) {
        Ok(()) => {
            state.channels.insert(login.clone(), None);
            svc.ui.send(UiEvent::Notice { channel: Some(login.clone()), text: format!("Dołączanie do #{login}…") });
        }
        Err(e) => svc.ui.send(UiEvent::Error {
            channel: Some(login.clone()),
            text: format!("Niepoprawna nazwa kanału „{login}”: {e}"),
        }),
    }
}

fn part_channel(svc: &Services, client: &AnonymousClient, state: &mut State, name: &str) {
    let login = normalize(name);
    if let Some(id) = state.channels.remove(&login) {
        client.part(login.clone());
        if let Some(id) = id {
            svc.store.remove_channel(&id);
            state.last_sent.remove(&id);
        }
    }
}

fn on_room_state(svc: &Services, state: &mut State, channel_login: String, channel_id: String) {
    // ROOMSTATE przychodzi też przy zmianie ustawień pokoju (slow mode,
    // emote-only…). Reagujemy tylko na pierwsze dla danego kanału.
    let Some(slot) = state.channels.get_mut(&channel_login) else { return };
    if slot.as_deref() == Some(channel_id.as_str()) {
        return;
    }
    *slot = Some(channel_id.clone());
    svc.store.begin_channel(&channel_id);
    svc.ui.send(UiEvent::Connected { channel: channel_login.clone(), channel_id: channel_id.clone() });

    let channel = ChannelRef { login: channel_login, id: channel_id };
    let s = svc.clone();
    let ch = channel.clone();
    tokio::spawn(async move {
        match s.emote_source.channel_emotes(&ch.id).await {
            Ok(emotes) => {
                let count = emotes.len();
                if s.store.set_channel(&ch.id, emotes) {
                    s.ui.send(UiEvent::EmotesLoaded { channel: Some(ch.login), count });
                }
            }
            Err(e) => s.ui.send(UiEvent::Error {
                channel: Some(ch.login),
                text: format!("{}: {e:#}", s.emote_source.name()),
            }),
        }
    });

    refresh_channel_context(svc, channel);
}

/// Dla zalogowanego użytkownika: uprawnienia moderatora i odznaki kanału.
fn refresh_channel_context(svc: &Services, channel: ChannelRef) {
    let svc = svc.clone();
    tokio::spawn(async move {
        if svc.tokens.user().await.is_none() {
            return;
        }
        match svc.helix.can_moderate(&channel.id).await {
            Ok(can_moderate) => svc.ui.send(UiEvent::ModStatus { channel: channel.login.clone(), can_moderate }),
            Err(e) => tracing::warn!("nie udało się sprawdzić uprawnień moderatora: {e}"),
        }
        match svc.helix.channel_badges(&channel.id).await {
            Ok(map) => svc.ui.send(UiEvent::Badges { channel: Some(channel.login), map: Arc::new(map) }),
            Err(e) => tracing::warn!("nie udało się pobrać odznak kanału: {e}"),
        }
    });
}

// ── Sesja ──────────────────────────────────────────────────────────────

fn start_login(svc: &Services, state: &mut State) {
    if let Some(task) = state.login_task.take() {
        task.abort();
    }
    let svc = svc.clone();
    state.login_task = Some(tokio::spawn(async move {
        let auth = svc.tokens.auth();
        let device = match auth.start_device_flow().await {
            Ok(d) => d,
            Err(e) => {
                svc.ui.send(UiEvent::error(format!("Logowanie nieudane: {e:#}")));
                svc.ui.send(UiEvent::Auth(AuthState::LoggedOut));
                return;
            }
        };

        svc.ui.send(UiEvent::Auth(AuthState::Pending {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            expires_at: Instant::now() + Duration::from_secs(device.expires_in),
        }));
        if let Err(e) = open::that_detached(&device.verification_uri) {
            tracing::warn!("nie udało się otworzyć przeglądarki: {e}");
        }

        match auth.poll_device_token(&device).await {
            Ok(token) => {
                let (user, warning) = svc.tokens.set(token).await;
                if let Some(w) = warning {
                    svc.ui.send(UiEvent::error(w));
                }
                on_logged_in(&svc, user);
            }
            Err(e) => {
                svc.ui.send(UiEvent::error(format!("Logowanie nieudane: {e:#}")));
                svc.ui.send(UiEvent::Auth(AuthState::LoggedOut));
            }
        }
    }));
}

fn on_logged_in(svc: &Services, user: UserInfo) {
    svc.ui.send(UiEvent::Auth(AuthState::LoggedIn { login: user.login, user_id: user.user_id.clone() }));
    let _ = svc.internal.send(Internal::LoggedIn);

    let svc = svc.clone();
    tokio::spawn(async move {
        match svc.helix.avatar_url(&user.user_id).await {
            Ok(Some(url)) => svc.ui.send(UiEvent::Avatar { url }),
            Ok(None) => {}
            Err(e) => tracing::warn!("nie udało się pobrać awatara: {e}"),
        }
        match svc.helix.global_badges().await {
            Ok(map) => svc.ui.send(UiEvent::Badges { channel: None, map: Arc::new(map) }),
            Err(e) => tracing::warn!("nie udało się pobrać globalnych odznak: {e}"),
        }
    });
}

async fn restore_session(svc: Services) {
    svc.ui.send(UiEvent::Auth(AuthState::Restoring));
    match svc.tokens.restore().await {
        Ok(Some(user)) => {
            // Token sprzed dodania nowych funkcji nie ma ich uprawnień —
            // Twitch nie pozwala ich „dopisać”, potrzebne jest nowe logowanie.
            let missing = svc.tokens.missing_scopes().await;
            if missing.is_empty() {
                on_logged_in(&svc, user);
            } else {
                tracing::info!(?missing, "zapisany token nie ma wymaganych uprawnień");
                svc.tokens.logout().await;
                svc.ui.send(UiEvent::Auth(AuthState::LoggedOut));
                svc.ui.send(UiEvent::notice(
                    "LiMBOCHAT potrzebuje nowych uprawnień Twitch. Zaloguj się ponownie.",
                ));
            }
        }
        Ok(None) => svc.ui.send(UiEvent::Auth(AuthState::LoggedOut)),
        Err(e) => {
            // Najczęściej: Linux bez gnome-keyring/KWallet. Szczegóły tylko w logu.
            tracing::warn!("sejf systemowy niedostępny: {e:#}");
            svc.ui.send(UiEvent::notice(
                "Sejf systemowy jest niedostępny — po zalogowaniu sesja nie zostanie zapamiętana po zamknięciu aplikacji.",
            ));
            svc.ui.send(UiEvent::Auth(AuthState::LoggedOut));
        }
    }
}

async fn periodic_validation(svc: Services) {
    let mut interval = tokio::time::interval(VALIDATION_INTERVAL);
    interval.tick().await; // pierwszy tick jest natychmiastowy — start obsługuje restore_session
    loop {
        interval.tick().await;
        if svc.tokens.user().await.is_none() {
            continue;
        }
        match svc.tokens.validate_now().await {
            Ok(Some(_)) => {}
            Ok(None) => {
                svc.ui.send(UiEvent::error("Sesja Twitch wygasła — zaloguj się ponownie."));
                svc.ui.send(UiEvent::Auth(AuthState::LoggedOut));
            }
            Err(e) => tracing::warn!("cogodzinna walidacja tokena nie powiodła się: {e:#}"),
        }
    }
}

// ── Czat ───────────────────────────────────────────────────────────────

fn send_message(svc: &Services, state: &mut State, channel: ChannelRef, text: String) {
    let mut text = text.trim().to_owned();
    if text.is_empty() {
        return;
    }

    // Identyczna wiadomość w ciągu 30 s zostałaby po cichu odrzucona przez Twitcha.
    let now = Instant::now();
    if let Some((last, at)) = state.last_sent.get(&channel.id) {
        if *last == text && now.duration_since(*at) < DUPLICATE_WINDOW {
            text.push_str(DUPLICATE_BYPASS);
        }
    }
    state.last_sent.insert(channel.id.clone(), (text.clone(), now));

    let svc = svc.clone();
    tokio::spawn(async move {
        // Sukces nie wymaga komunikatu: wiadomość wróci do nas przez IRC
        // i pojawi się na czacie jak każda inna.
        if let Err(e) = svc.helix.send_chat_message(&channel.id, &text).await {
            report_helix_error(&svc, &channel, "wysłanie wiadomości", e);
        }
    });
}

// ── Moderacja ──────────────────────────────────────────────────────────

fn moderate(svc: &Services, cmd: Command) {
    let svc = svc.clone();
    tokio::spawn(async move {
        let helix = &svc.helix;
        // Sukces bana/timeoutu/usunięcia potwierdzi IRC (CLEARCHAT/CLEARMSG),
        // więc komunikat własny wysyłamy tylko dla unbana.
        let (channel, action, result, confirmation) = match cmd {
            Command::Timeout { channel, user_id, login, seconds, reason } => {
                let r = helix.ban_user(&channel.id, &user_id, Some(seconds), reason.as_deref()).await;
                (channel, format!("timeout dla {login}"), r, None)
            }
            Command::Ban { channel, user_id, login, reason } => {
                let r = helix.ban_user(&channel.id, &user_id, None, reason.as_deref()).await;
                (channel, format!("ban dla {login}"), r, None)
            }
            Command::Unban { channel, user_id, login } => {
                let r = helix.unban_user(&channel.id, &user_id).await;
                (channel, format!("odbanowanie {login}"), r, Some(format!("{login} został odbanowany")))
            }
            Command::DeleteMessage { channel, message_id } => {
                let r = helix.delete_message(&channel.id, &message_id).await;
                (channel, "usunięcie wiadomości".to_owned(), r, None)
            }
            _ => return,
        };

        match result {
            Ok(()) => {
                if let Some(text) = confirmation {
                    svc.ui.send(UiEvent::Notice { channel: Some(channel.login), text });
                }
            }
            Err(e) => report_helix_error(&svc, &channel, &action, e),
        }
    });
}

fn report_helix_error(svc: &Services, channel: &ChannelRef, action: &str, error: HelixError) {
    if matches!(error, HelixError::SessionExpired) {
        svc.ui.send(UiEvent::error("Sesja wygasła — zaloguj się ponownie."));
        svc.ui.send(UiEvent::Auth(AuthState::LoggedOut));
        return;
    }
    svc.ui.send(UiEvent::Error { channel: Some(channel.login.clone()), text: format!("Nie udało się: {action} — {error}") });
}

async fn load_global_emotes(svc: Services) {
    // Ponawianie z rosnącym odstępem — chwilowy błąd sieci nie powinien
    // zabrać globalnych emotek na całą sesję.
    let mut delay = Duration::from_secs(2);
    for attempt in 1..=5 {
        match svc.emote_source.global_emotes().await {
            Ok(emotes) => {
                let count = emotes.len();
                svc.store.set_global(emotes);
                svc.ui.send(UiEvent::EmotesLoaded { channel: None, count });
                return;
            }
            Err(e) => {
                tracing::warn!(attempt, "nie udało się pobrać globalnych emotek: {e:#}");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }
    svc.ui.send(UiEvent::error(format!("{}: nie udało się pobrać globalnych emotek", svc.emote_source.name())));
}
