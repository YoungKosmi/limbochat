// Rozszerzenie aktora Hub. Dane gry nie są nigdy poleceniami dla modelu AI.
use crate::studio::{self, Command as StudioCommand};
use serde_json::{Value, json};

pub enum StudioResult {
    Ballots {
        channel: String,
        result: Result<studio::Ballots, String>,
    },
    Replay {
        id: String,
        result: Result<String, String>,
    },
    Title {
        id: String,
        title: Option<String>,
    },
    Sources(Result<Vec<String>, String>),
    Frame {
        generation: u64,
        source: String,
        result: Result<String, String>,
    },
    Vision {
        generation: u64,
        result: Result<String, String>,
    },
    Playback(Result<(), String>),
}

impl Hub {
    fn studio_public(&self) -> Value {
        json!({"quiz":self.quiz.as_ref().map(crate::games::Quiz::public),"boss":self.boss.as_ref().map(crate::games::Boss::public),"ballot":self.ballot_overlay})
    }
    fn studio_state(&self) -> Value {
        let mut value = self.studio.public();
        value["public"] = self.studio_public();
        value["busy"] = json!(self.studio_busy.iter().collect::<Vec<_>>());
        value
    }
    fn studio_event(&self, event: &str, data: Value) {
        self.broadcast(&ServerMsg::Studio {
            event: event.into(),
            data,
        });
    }
    fn studio_changed(&mut self) {
        self.studio.save();
        self.studio_event("state", self.studio_state());
    }
    fn games_changed(&self) {
        self.studio_event("public", self.studio_public());
    }
    fn studio_say(&self, text: String, voice: bool) {
        self.broadcast(&ServerMsg::Jarvis {
            ev: jarvis::JarvisEvent::Reply { text: text.clone() },
        });
        if voice {
            if let Some(j) = &self.jarvis {
                j.say(text);
            }
        }
    }
    fn tick_games(&mut self) {
        let now = studio::now();
        let quiz = self.quiz.as_mut().is_some_and(|q| q.tick(now));
        let boss = self.boss.as_mut().is_some_and(|b| b.tick(now));
        if quiz {
            if let Some(q) = &self.quiz {
                self.studio_say(q.narration(), q.voice);
            }
        }
        if boss {
            let voice = self.boss.as_ref().is_some_and(|b| b.voice);
            self.studio_say(
                "Czas minął! Boss wygrał tę rundę. Następnym razem damy radę!".into(),
                voice,
            );
        }
        if quiz || boss || self.games_dirty {
            self.games_dirty = false;
            self.games_changed();
        }
    }
    fn studio_chat(&mut self, msg: &ParsedMessage) {
        let now = studio::now();
        let at = msg.timestamp.timestamp_millis();
        self.studio.chat(
            &msg.channel_login,
            studio::ChatLine {
                at,
                login: msg.sender_login.clone(),
                name: msg.sender_name.clone(),
                text: studio::text(&msg.raw_text, 500),
            },
        );
        let quiz = self.quiz.as_mut().is_some_and(|q| {
            q.message(
                &msg.channel_login,
                &msg.sender_login,
                &msg.sender_name,
                &msg.raw_text,
                at,
                now,
            )
        });
        let previous = self.boss.as_ref().map(crate::games::Boss::phase);
        let boss = self.boss.as_mut().is_some_and(|b| {
            b.message(
                &msg.channel_login,
                &msg.sender_login,
                &msg.sender_name,
                &msg.raw_text,
                at,
                now,
            )
        });
        if boss
            && previous == Some("battle")
            && self.boss.as_ref().is_some_and(|b| b.phase() == "victory")
        {
            let b = self.boss.as_ref().unwrap();
            let scene = b.victory_scene.clone();
            let voice = b.voice;
            self.studio_say("Boss pokonany! Czat wygrywa!".into(), voice);
            if !scene.is_empty() {
                let obs = self.obs.clone();
                let tx = self.ui_tx.clone();
                tokio::spawn(async move {
                    if let Err(text) = obs
                        .request("SetCurrentProgramScene", json!({"sceneName":scene}))
                        .await
                    {
                        let _ = tx.send(UiEvent::Error {
                            channel: None,
                            text,
                        });
                    }
                });
            }
        }
        if quiz || boss {
            self.games_dirty = true;
        }
    }
    fn studio_job(&mut self, name: &'static str) -> Result<(), String> {
        if !self.studio_busy.insert(name) {
            return Err("Poprzednia operacja jeszcze trwa.".into());
        }
        self.studio_event("state", self.studio_state());
        Ok(())
    }

    fn studio_command(
        &mut self,
        command: StudioCommand,
        conn: Option<&Outbox>,
    ) -> Result<Value, String> {
        match command {
            StudioCommand::Get => {
                if let Some(conn) = conn {
                    Self::reply(
                        conn,
                        &ServerMsg::Studio {
                            event: "state".into(),
                            data: self.studio_state(),
                        },
                    );
                }
                return Ok(self.studio_state());
            }
            StudioCommand::Viewer { channel, login } => {
                let data = self.studio.viewer(&channel, &login);
                if let Some(conn) = conn {
                    Self::reply(
                        conn,
                        &ServerMsg::Studio {
                            event: "viewer".into(),
                            data: data.clone(),
                        },
                    );
                }
                return Ok(data);
            }
            StudioCommand::ViewerNote {
                channel,
                login,
                notes,
            } => {
                let key = studio::viewer_key(&channel, &login);
                let notes = studio::text(&notes, 2000);
                if notes.is_empty() {
                    self.studio.data.notes.remove(&key);
                } else {
                    if self.studio.data.notes.len() >= 1000
                        && !self.studio.data.notes.contains_key(&key)
                    {
                        return Err("Limit notatek o 1000 widzach.".into());
                    }
                    self.studio.data.notes.insert(key, notes);
                }
                self.studio.dirty = true;
                self.studio.save();
                return self.studio_command(StudioCommand::Viewer { channel, login }, conn);
            }
            StudioCommand::Moment { label, replay } => {
                let channel = self
                    .config
                    .active
                    .clone()
                    .or_else(|| self.channels.first().map(|c| c.login.clone()))
                    .ok_or("Otwórz kanał przed zapisaniem momentu.")?;
                if replay {
                    self.studio_job("replay")?;
                }
                let chat = self
                    .channels
                    .iter()
                    .find(|c| c.login == channel)
                    .map(|c| {
                        c.history
                            .iter()
                            .rev()
                            .take(20)
                            .rev()
                            .map(|m| studio::ChatLine {
                                at: m.timestamp.timestamp_millis(),
                                login: m.sender_login.clone(),
                                name: m.sender_name.clone(),
                                text: studio::text(&m.raw_text, 500),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let label = if label.trim().is_empty() {
                    "Ale była akcja!".into()
                } else {
                    studio::text(&label, 140)
                };
                let id = studio::uid();
                let elapsed = self
                    .stats
                    .as_ref()
                    .filter(|s| s.channel == channel)
                    .map(|s| (chrono::Utc::now() - s.started_at).num_seconds().max(0));
                self.studio.data.moments.insert(
                    0,
                    studio::Moment {
                        id: id.clone(),
                        at: studio::now(),
                        channel,
                        label: label.clone(),
                        elapsed,
                        chat,
                        replay: None,
                        replay_error: None,
                        title: Some(label.clone()),
                    },
                );
                self.studio.data.moments.truncate(200);
                self.studio.dirty = true;
                if replay {
                    let obs = self.obs.clone();
                    let me = self.me.clone();
                    let moment_id = id.clone();
                    tokio::spawn(async move {
                        let result = obs.capture_replay().await;
                        let _ = me.requests.send(Request::StudioDone(StudioResult::Replay {
                            id: moment_id,
                            result,
                        }));
                    });
                }
                if self.config.jarvis.enabled {
                    if let Some(j) = &self.jarvis {
                        let j = j.clone();
                        let me = self.me.clone();
                        let moment_id = id.clone();
                        tokio::spawn(async move {
                            let title=j.ask("Zaproponuj krótki polski tytuł klipu (maksymalnie 70 znaków), bez cudzysłowów. Nie dodawaj nieznanych faktów.",label).await.ok().flatten();
                            let _ = me.requests.send(Request::StudioDone(StudioResult::Title {
                                id: moment_id,
                                title,
                            }));
                        });
                    }
                }
                self.studio_changed();
                return Ok(json!({"id":id,"saved":true,"replay_pending":replay}));
            }
            StudioCommand::MomentDelete { id } => {
                self.studio.data.moments.retain(|m| m.id != id);
                self.studio.dirty = true;
            }
            StudioCommand::MomentTitle { id, title } => {
                let m = self
                    .studio
                    .data
                    .moments
                    .iter_mut()
                    .find(|m| m.id == id)
                    .ok_or("Nie ma tego momentu.")?;
                m.title = Some(studio::text(&title, 100));
                self.studio.dirty = true;
            }
            StudioCommand::ReplaySource { source } => {
                self.studio.data.replay_source = studio::text(&source, 200);
                self.studio.dirty = true;
            }
            StudioCommand::ReplayStop => {
                if let Some(stop) = self.replay_stop.take() {
                    let _ = stop.send(true);
                }
                return Ok(json!({"stopping":true}));
            }
            StudioCommand::ReplayShow { id } => {
                let path = self
                    .studio
                    .data
                    .moments
                    .iter()
                    .find(|m| m.id == id)
                    .and_then(|m| m.replay.clone())
                    .ok_or("Ten moment nie ma zapisanej powtórki.")?;
                let source = self.studio.data.replay_source.clone();
                if source.is_empty() {
                    return Err("Wskaż źródło multimedialne OBS do odtwarzania powtórek.".into());
                }
                self.studio_job("playback")?;
                let (stop, rx) = watch::channel(false);
                self.replay_stop = Some(stop);
                let obs = self.obs.clone();
                let me = self.me.clone();
                tokio::spawn(async move {
                    let result = crate::studio::play_replay(obs, source, path, rx).await;
                    let _ = me
                        .requests
                        .send(Request::StudioDone(StudioResult::Playback(result)));
                });
                return Ok(json!({"playing":true}));
            }
            StudioCommand::ProfileSave { name } => {
                let name = studio::text(&name, 40);
                if name.is_empty() {
                    return Err("Podaj nazwę profilu.".into());
                }
                if self.studio.data.profiles.len() >= 12
                    && !self.studio.data.profiles.iter().any(|p| p.name == name)
                {
                    return Err("Limit 12 profili.".into());
                }
                self.studio.data.profiles.retain(|p| p.name != name);
                self.studio.data.profiles.push(studio::Profile::capture(
                    &name,
                    &self.config,
                    &self.studio.data.vision,
                ));
                self.studio.data.active_profile = Some(name);
                self.studio.dirty = true;
            }
            StudioCommand::ProfileDelete { name } => {
                self.studio.data.profiles.retain(|p| p.name != name);
                if self.studio.data.active_profile.as_ref() == Some(&name) {
                    self.studio.data.active_profile = None;
                }
                self.studio.dirty = true;
            }
            StudioCommand::ProfileApply { name } => {
                let p = self
                    .studio
                    .data
                    .profiles
                    .iter()
                    .find(|p| p.name == name)
                    .cloned()
                    .ok_or("Nie ma tego profilu.")?;
                p.apply(&mut self.config);
                self.studio.data.vision = p.vision;
                self.vision_generation += 1;
                self.vision_frame = None;
                self.studio.data.active_profile = Some(name);
                self.studio.dirty = true;
                self.save_config();
                if let Some(j) = &self.jarvis {
                    j.set_config(self.config.jarvis.clone());
                }
                self.restart_eventsub();
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
            StudioCommand::VisionSettings { mut config } => {
                config.source = studio::text(&config.source, 200);
                if config.enabled && config.source.is_empty() {
                    return Err("Wybierz jedno źródło OBS.".into());
                }
                self.vision_generation += 1;
                self.vision_frame = None;
                self.studio.data.vision = config;
                self.studio.dirty = true;
            }
            StudioCommand::VisionSources => {
                self.studio_job("sources")?;
                let obs = self.obs.clone();
                let me = self.me.clone();
                tokio::spawn(async move {
                    let result = async {
                        let inputs = obs.request("GetInputList", json!({})).await?;
                        let mut names: Vec<String> = inputs["inputs"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v["inputName"].as_str().map(str::to_owned))
                            .collect();
                        names.extend(obs.status().scenes);
                        names.sort();
                        names.dedup();
                        Ok(names)
                    }
                    .await;
                    let _ = me
                        .requests
                        .send(Request::StudioDone(StudioResult::Sources(result)));
                });
                return Ok(json!({"loading":true}));
            }
            StudioCommand::VisionCapture => {
                if !self.studio.data.vision.enabled {
                    return Err("Włącz analizę obrazu i wybierz źródło OBS.".into());
                }
                self.studio_job("capture")?;
                self.vision_frame = None;
                let source = self.studio.data.vision.source.clone();
                let obs = self.obs.clone();
                let me = self.me.clone();
                let generation = self.vision_generation;
                tokio::spawn(async move {
                    let result=obs.request("GetSourceScreenshot",json!({"sourceName":source,"imageFormat":"jpeg","imageWidth":960,"imageCompressionQuality":70})).await.and_then(|v|v["imageData"].as_str().map(str::to_owned).ok_or("OBS nie zwrócił kadru.".into())).and_then(|data|crate::vision::image_data(&data).map(|_|data.clone()));
                    let _ = me.requests.send(Request::StudioDone(StudioResult::Frame {
                        generation,
                        source,
                        result,
                    }));
                });
                return Ok(json!({"capturing":true}));
            }
            StudioCommand::VisionAnalyze { question } => {
                if !self.studio.data.vision.enabled {
                    return Err("Analiza obrazu jest wyłączona.".into());
                }
                let (source, image, at) = self
                    .vision_frame
                    .clone()
                    .ok_or("Najpierw pobierz i obejrzyj kadr.")?;
                if source != self.studio.data.vision.source || studio::now() - at > 120_000 {
                    return Err("Kadr wygasł. Pobierz nowy podgląd.".into());
                }
                self.studio_job("vision")?;
                let config = self.config.jarvis.clone();
                let me = self.me.clone();
                let generation = self.vision_generation;
                tokio::spawn(async move {
                    let result = tokio::time::timeout(
                        Duration::from_secs(45),
                        crate::vision::analyze(config, image, question),
                    )
                    .await
                    .unwrap_or_else(|_| Err("Analiza przekroczyła 45 sekund.".into()));
                    let _ = me.requests.send(Request::StudioDone(StudioResult::Vision {
                        generation,
                        result,
                    }));
                });
                return Ok(json!({"analyzing":true}));
            }
            StudioCommand::QuestionsSave { questions } => {
                crate::games::validate(&questions)?;
                self.studio.data.questions = questions;
                self.studio.dirty = true;
            }
            StudioCommand::QuizStart {
                channel,
                seconds,
                voice,
            } => {
                self.require_game_channel(&channel)?;
                self.quiz = Some(crate::games::Quiz::new(
                    channel,
                    self.studio.data.questions.clone(),
                    seconds,
                    voice,
                    studio::now(),
                )?);
                let q = self.quiz.as_ref().unwrap();
                self.studio_say(q.narration(), q.voice);
                self.games_changed();
            }
            StudioCommand::QuizNext => {
                if let Some(q) = &mut self.quiz {
                    q.next(studio::now());
                    let (text, voice) = (q.narration(), q.voice);
                    self.studio_say(text, voice);
                }
                self.games_changed();
            }
            StudioCommand::QuizStop => {
                self.quiz = None;
                self.games_changed();
            }
            StudioCommand::BossStart {
                channel,
                name,
                hp,
                seconds,
                voice,
                victory_scene,
            } => {
                self.require_game_channel(&channel)?;
                if !victory_scene.is_empty() && !self.obs.status().scenes.contains(&victory_scene) {
                    return Err("Wybierz istniejącą scenę OBS dla zwycięstwa.".into());
                }
                self.boss = Some(crate::games::Boss::new(
                    channel,
                    name,
                    hp,
                    seconds,
                    voice,
                    victory_scene,
                    self.studio.data.questions.clone(),
                    studio::now(),
                )?);
                self.studio_say("Czat kontra boss! Wysyłaj !atak i literę poprawnej odpowiedzi, na przykład !atak A.".into(),voice);
                self.games_changed();
            }
            StudioCommand::BossStop => {
                self.boss = None;
                self.games_changed();
            }
            StudioCommand::BallotShow {
                kind,
                index,
                announce,
            } => {
                if !matches!(kind.as_str(), "off" | "poll" | "prediction") {
                    return Err("Nieznany rodzaj głosowania.".into());
                }
                if kind == "off" {
                    self.ballot_overlay = None;
                } else {
                    let (channel, _) = self.ballot_owner()?;
                    let ballots = self
                        .studio
                        .data
                        .ballots
                        .get(&channel)
                        .ok_or("Najpierw odśwież wyniki.")?;
                    let rows = if kind == "poll" {
                        &ballots.polls
                    } else {
                        &ballots.predictions
                    };
                    let entry = rows.get(index).ok_or("Nie ma takiego głosowania.")?;
                    let entry = entry.clone();
                    self.ballot_overlay =
                        Some(json!({"kind":kind,"channel":channel,"entry":entry}));
                    if announce {
                        self.studio_say(
                            format!(
                                "Wynik głosowania: {}. {}",
                                entry["title"].as_str().unwrap_or(""),
                                entry["winner"]
                                    .as_str()
                                    .map(|winner| format!("Wygrywa {winner}."))
                                    .unwrap_or_else(|| "Nie ma jeszcze zwycięzcy.".into())
                            ),
                            true,
                        );
                    }
                }
                self.games_changed();
            }
            command @ (StudioCommand::BallotsRefresh
            | StudioCommand::PollCreate { .. }
            | StudioCommand::PollEnd
            | StudioCommand::PredictionCreate { .. }
            | StudioCommand::PredictionEnd { .. }) => {
                let (channel, id) = self.ballot_owner()?;
                validate_ballot(&command)?;
                self.studio_job("ballots")?;
                let me = self.me.clone();
                let helix = self.helix.clone();
                let demo = self.demo.is_some();
                let previous = self.studio.data.ballots.get(&channel).cloned();
                tokio::spawn(async move {
                    let result = if demo {
                        demo_ballots(&channel, command, previous)
                    } else {
                        match helix {
                            Some(h) => ballot_request(h, &channel, &id, command).await,
                            None => Err("Zaloguj Twitcha.".into()),
                        }
                    };
                    let _ = me.requests.send(Request::StudioDone(StudioResult::Ballots {
                        channel,
                        result,
                    }));
                });
                return Ok(json!({"loading":true}));
            }
        }
        self.studio_changed();
        Ok(json!({"ok":true}))
    }
    fn require_game_channel(&self, channel: &str) -> Result<(), String> {
        if self.channels.iter().any(|c| c.login == channel) {
            Ok(())
        } else {
            Err("Otwórz kanał, na którym ma działać gra.".into())
        }
    }
    fn ballot_owner(&self) -> Result<(String, String), String> {
        match &self.auth {
            AuthState::LoggedIn { login, user_id } => Ok((login.clone(), user_id.clone())),
            _ => Err("Zaloguj konto nadawcy ankiet i predykcji.".into()),
        }
    }
    fn studio_done(&mut self, result: StudioResult) {
        let mut error = None;
        match result {
            StudioResult::Ballots { channel, result } => {
                self.studio_busy.remove("ballots");
                match result {
                    Ok(ballots) => {
                        if let Some(shown) = self
                            .ballot_overlay
                            .as_mut()
                            .filter(|s| s["channel"] == channel)
                        {
                            let rows = if shown["kind"] == "poll" {
                                &ballots.polls
                            } else {
                                &ballots.predictions
                            };
                            if let Some(entry) = rows.iter().find(|entry| {
                                (!entry["id"].is_null() && entry["id"] == shown["entry"]["id"])
                                    || entry["title"] == shown["entry"]["title"]
                            }) {
                                shown["entry"] = entry.clone();
                            }
                        }
                        self.studio.data.ballots.insert(channel, ballots);
                        self.studio.dirty = true;
                        self.games_changed();
                    }
                    Err(e) => error = Some(e),
                }
            }
            StudioResult::Replay { id, result } => {
                self.studio_busy.remove("replay");
                if let Some(m) = self.studio.data.moments.iter_mut().find(|m| m.id == id) {
                    match result {
                        Ok(path) => m.replay = Some(path),
                        Err(e) => {
                            m.replay_error = Some(e.clone());
                            error = Some(e)
                        }
                    }
                    self.studio.dirty = true;
                }
            }
            StudioResult::Title { id, title } => {
                if let (Some(m), Some(title)) = (
                    self.studio.data.moments.iter_mut().find(|m| m.id == id),
                    title,
                ) {
                    m.title = Some(studio::text(&title, 100));
                    self.studio.dirty = true;
                }
            }
            StudioResult::Sources(result) => {
                self.studio_busy.remove("sources");
                match result {
                    Ok(sources) => self.studio_event("sources", json!(sources)),
                    Err(e) => error = Some(e),
                }
            }
            StudioResult::Frame {
                generation,
                source,
                result,
            } => {
                self.studio_busy.remove("capture");
                if generation == self.vision_generation {
                    match result {
                        Ok(image) => {
                            self.vision_frame = Some((source, image.clone(), studio::now()));
                            self.studio_event("frame", json!({"image":image}));
                        }
                        Err(e) => error = Some(e),
                    }
                }
            }
            StudioResult::Vision { generation, result } => {
                self.studio_busy.remove("vision");
                if generation == self.vision_generation && self.studio.data.vision.enabled {
                    match result {
                        Ok(text) => {
                            self.studio_event("vision", json!({"text":text}));
                            self.studio_say(text, self.studio.data.vision.speak);
                        }
                        Err(e) => error = Some(e),
                    }
                }
            }
            StudioResult::Playback(result) => {
                self.studio_busy.remove("playback");
                self.replay_stop = None;
                if let Err(e) = result {
                    error = Some(e);
                }
            }
        }
        if let Some(error) = error {
            self.notice(None, error.clone(), true);
            self.studio_event("error", json!({"text":error}));
        }
        self.studio_changed();
    }
}

fn validate_ballot(command: &StudioCommand) -> Result<(), String> {
    match command {
        StudioCommand::PollCreate {
            title,
            choices,
            seconds,
        }
        | StudioCommand::PredictionCreate {
            title,
            choices,
            seconds,
        } => {
            let poll = matches!(command, StudioCommand::PollCreate { .. });
            let title_limit = if poll { 60 } else { 45 };
            let max = if poll { 5 } else { 10 };
            let min = if poll { 15 } else { 30 };
            if title.trim().is_empty()
                || title.chars().count() > title_limit
                || !(2..=max).contains(&choices.len())
                || choices
                    .iter()
                    .any(|c| c.trim().is_empty() || c.chars().count() > 25)
                || !((min)..=1800).contains(seconds)
            {
                return Err("Sprawdź tytuł, 2–5 odpowiedzi ankiety / 2–10 predykcji (do 25 znaków) oraz czas 15/30–1800 sekund.".into());
            }
        }
        StudioCommand::PredictionEnd { mode, winner } => {
            if !matches!(mode.as_str(), "lock" | "cancel" | "resolve")
                || (mode == "resolve" && winner.as_ref().is_none_or(|s| s.trim().is_empty()))
            {
                return Err("Wybierz działanie i zwycięską odpowiedź predykcji.".into());
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod studio_validation_tests {
    use super::*;
    #[test]
    fn invalid_poll_inputs_never_reach_twitch() {
        for choices in [vec![], vec!["a".into()], vec!["x".repeat(26), "b".into()]] {
            assert!(
                validate_ballot(&StudioCommand::PollCreate {
                    title: "x".into(),
                    choices,
                    seconds: 60
                })
                .is_err()
            );
        }
        assert!(
            validate_ballot(&StudioCommand::PredictionEnd {
                mode: "resolve".into(),
                winner: None
            })
            .is_err()
        );
    }
}
async fn ballot_request(
    helix: Arc<Helix>,
    channel: &str,
    id: &str,
    command: StudioCommand,
) -> Result<studio::Ballots, String> {
    use crate::jarvis::twitch::PredictionEnd;
    match command {
        StudioCommand::PollCreate {
            title,
            choices,
            seconds,
        } => helix
            .create_poll(id, &title, &choices, seconds)
            .await
            .map_err(|e| e.to_string())?,
        StudioCommand::PollEnd => {
            helix.end_poll(id).await.map_err(|e| e.to_string())?;
        }
        StudioCommand::PredictionCreate {
            title,
            choices,
            seconds,
        } => helix
            .create_prediction(id, &title, &choices, seconds)
            .await
            .map_err(|e| e.to_string())?,
        StudioCommand::PredictionEnd { mode, winner } => {
            helix
                .end_prediction(
                    id,
                    match mode.as_str() {
                        "lock" => PredictionEnd::Lock,
                        "cancel" => PredictionEnd::Cancel,
                        _ => PredictionEnd::Resolve,
                    },
                    winner.as_deref(),
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        _ => {}
    }
    let (polls, predictions) = tokio::join!(helix.poll_history(id), helix.prediction_history(id));
    Ok(studio::Ballots {
        channel: channel.into(),
        polls: polls.map_err(|e| e.to_string())?,
        predictions: predictions.map_err(|e| e.to_string())?,
        updated: studio::now(),
    })
}
fn demo_ballots(
    channel: &str,
    command: StudioCommand,
    previous: Option<studio::Ballots>,
) -> Result<studio::Ballots, String> {
    let mut result=previous.unwrap_or(studio::Ballots{channel:channel.into(),polls:vec![json!({"title":"Co gramy?","status":"zakończona","status_raw":"COMPLETED","winner":"Minecraft","total_votes":100,"choices":[{"title":"Minecraft","votes":65,"percent":65},{"title":"Portal","votes":35,"percent":35}]})],predictions:vec![json!({"title":"Czy pokonamy bossa?","status":"rozstrzygnięta","status_raw":"RESOLVED","winner":"Tak","outcomes":[{"title":"Tak","users":30,"channel_points":5000},{"title":"Nie","users":12,"channel_points":2000}]})],updated:studio::now()});
    match command{
        StudioCommand::PollCreate{title,choices,..}=>result.polls.insert(0,json!({"title":title,"status":"trwa","status_raw":"ACTIVE","choices":choices.iter().map(|c|json!({"title":c,"votes":0,"percent":0})).collect::<Vec<_>>() })),
        StudioCommand::PredictionCreate{title,choices,..}=>result.predictions.insert(0,json!({"title":title,"status":"trwa","status_raw":"ACTIVE","outcomes":choices.iter().map(|c|json!({"title":c,"users":0,"channel_points":0})).collect::<Vec<_>>() })),
        StudioCommand::PollEnd=>if let Some(row)=result.polls.iter_mut().find(|r|r["status_raw"]=="ACTIVE"){row["status_raw"]=json!("TERMINATED");row["status"]=json!("zakończona");},
        StudioCommand::PredictionEnd{mode,winner}=>if let Some(row)=result.predictions.iter_mut().find(|r|r["status_raw"]=="ACTIVE"||r["status_raw"]=="LOCKED"){row["status_raw"]=json!(match mode.as_str(){"lock"=>"LOCKED","cancel"=>"CANCELED",_=>"RESOLVED"});row["status"]=row["status_raw"].clone();row["winner"]=json!(winner);},
        _=>{}
    }
    result.polls.truncate(20);
    result.predictions.truncate(20);
    result.updated = studio::now();
    Ok(result)
}
