//! Hub: jedno zadanie (aktor), które posiada cały stan panelu.
//!
//! * Odbiera zdarzenia silnika (`UiEvent`) → aktualizuje stan → rozsyła
//!   JSON do wszystkich otwartych paneli (broadcast).
//! * Odbiera komendy z paneli → przekazuje je silnikowi (`Command`).
//! * Nowy panel (lub przeładowany dock) dostaje migawkę stanu: kanały,
//!   ostatnie wiadomości, logowanie, odznaki — więc odświeżenie panelu
//!   w OBS niczego nie gubi.

use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::{
    runtime::Handle,
    sync::{broadcast, mpsc, oneshot, watch},
};

use crate::{
    backend::{self, BackendConfig},
    jarvis::{self, JarvisHandle, JarvisStatus},
    config::{self, Config},
    demo,
    emotes::{EmoteMap, EmoteScale, EmoteStore},
    eventsub,
    events::{AlertKind, AuthState, ChannelRef, ChatAlert, Command, UiEvent, UiSender},
    helix::{BadgeMap, Helix},
    parser::{self, ParsedMessage},
    protocol::{AlertDto, AuthDto, ChannelDto, ClientMsg, EmoteDto, MessageDto, ServerMsg, Snapshot, badges_dto},
    update::{UpdateStatus, UpdaterHandle},
    changelog,
    memory::{Forget, Memory},
    obs::{self, ObsEvent, ObsHandle},
    summary::{self, Stats, Summary},
};

/// Ile ostatnich wiadomości na kanał trzymamy dla przeładowanych paneli.
const HISTORY: usize = 300;
include!("studio_hub.rs");

pub type Outbox = mpsc::UnboundedSender<Arc<str>>;

pub enum Request {
    StudioCommand { command:crate::studio::Command, reply:oneshot::Sender<Result<serde_json::Value,String>> },
    StudioDone(StudioResult),
    /// Nowe połączenie panelu: odpowiedź = migawka + subskrypcja zdarzeń.
    Connect { reply: oneshot::Sender<(Arc<str>, broadcast::Receiver<Arc<str>>)> },
    Client { msg: ClientMsg, conn: Outbox },
    /// Jarvis prosi o migawkę kontekstu (kanał, czat, logowanie).
    JarvisContext { reply: oneshot::Sender<jarvis::tools::Context> },
    /// Jarvis zmienia kolor panelu (głosem).
    SetAccent { accent: String },
    /// Jarvis otwiera lub przełącza kanał w panelu.
    OpenChannel { login: String, reply: oneshot::Sender<Result<String, String>> },
    /// Przed aktualizacją: historia czatu dla nowej wersji silnika.
    Export { reply: oneshot::Sender<Handover> },
    /// Czy można teraz zaktualizować: (tryb automatyczny, nic się nie dzieje).
    UpdateGate { reply: oneshot::Sender<(bool, bool)> },
    /// Nowy stan aktualizacji — do wszystkich paneli.
    UpdateStatus(UpdateStatus),
    /// Streamer kazał LimboAI zainstalować aktualizację (gdy wszystko ucichnie).
    InstallUpdate,
    /// LimboAI zrobił klip (do podsumowania streama).
    ClipMade(String),
    /// Podsumowanie słowami / tytuł streama dopisane po zakończeniu.
    SummaryDone { id: u64, text: Option<String>, title: Option<String>, category: Option<String> },
    /// Fakty o widzach wyciągnięte przez LimboAI z czatu.
    MemoryFacts(Vec<(String, String)>),
    /// Stan streama z Twitcha (co 2 min): Some(widzowie) = na żywo.
    TwitchLive { viewers: Option<u64>, started_at: Option<chrono::DateTime<chrono::Utc>> },
    /// Rymowanka do alertu gotowa (albo `None` — nie udało się).
    AlertRhyme { channel: String, id: String, rhyme: Option<String> },
}

/// Czat przekazywany nowej wersji silnika przy aktualizacji.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Handover {
    pub channels: Vec<(String, Vec<MessageDto>)>,
}

/// Połączenie huba z resztą silnika.
#[derive(Default)]
pub struct EngineLink {
    pub updater: Option<UpdaterHandle>,
    pub handover: Option<Handover>,
}

#[derive(Clone)]
pub struct HubHandle {
    requests: mpsc::UnboundedSender<Request>,
}

impl HubHandle {
    pub async fn studio_command(&self,command:crate::studio::Command)->Result<serde_json::Value,String>{
        let(reply,rx)=oneshot::channel();self.requests.send(Request::StudioCommand{command,reply}).map_err(|_|"Panel nie działa.".to_owned())?;rx.await.map_err(|_|"Panel nie odpowiedział.".to_owned())?
    }
    pub async fn connect(&self) -> Option<(Arc<str>, broadcast::Receiver<Arc<str>>)> {
        let (reply, rx) = oneshot::channel();
        self.requests.send(Request::Connect { reply }).ok()?;
        rx.await.ok()
    }

    pub fn client(&self, msg: ClientMsg, conn: Outbox) {
        let _ = self.requests.send(Request::Client { msg, conn });
    }

    pub fn set_accent(&self, accent: String) {
        let _ = self.requests.send(Request::SetAccent { accent });
    }

    pub async fn open_channel(&self, login: String) -> Result<String, String> {
        let (reply, rx) = oneshot::channel();
        self.requests.send(Request::OpenChannel { login, reply }).map_err(|_| "panel nie działa".to_owned())?;
        rx.await.map_err(|_| "panel nie odpowiada".to_owned())?
    }

    pub async fn export(&self) -> Handover {
        let (reply, rx) = oneshot::channel();
        if self.requests.send(Request::Export { reply }).is_err() {
            return Handover::default();
        }
        rx.await.unwrap_or_default()
    }

    /// (aktualizować automatycznie?, czy LimboAI i panel są bezczynne?)
    pub async fn update_gate(&self) -> (bool, bool) {
        let (reply, rx) = oneshot::channel();
        if self.requests.send(Request::UpdateGate { reply }).is_err() {
            return (false, false);
        }
        rx.await.unwrap_or((false, false))
    }

    pub fn clip_made(&self, url: String) {
        let _ = self.requests.send(Request::ClipMade(url));
    }

    pub fn update_status(&self, status: UpdateStatus) {
        let _ = self.requests.send(Request::UpdateStatus(status));
    }

    pub fn install_update(&self) {
        let _ = self.requests.send(Request::InstallUpdate);
    }

    pub async fn jarvis_context(&self) -> Option<jarvis::tools::Context> {
        let (reply, rx) = oneshot::channel();
        self.requests.send(Request::JarvisContext { reply }).ok()?;
        rx.await.ok()
    }
}

struct Channel {
    login: String,
    id: Option<String>,
    can_moderate: bool,
    emote_count: usize,
    badges: Arc<BadgeMap>,
    history: VecDeque<ParsedMessage>,
    /// Wiadomości przejęte od poprzedniej wersji silnika (po aktualizacji).
    carried: VecDeque<MessageDto>,
    /// Alerty (bitsy, suby, raidy…) — ostatnie 40.
    alerts: VecDeque<MessageDto>,
}

impl Channel {
    fn new(login: String) -> Self {
        Self {
            login,
            id: None,
            can_moderate: false,
            emote_count: 0,
            badges: Arc::default(),
            history: VecDeque::new(),
            carried: VecDeque::new(),
            alerts: VecDeque::new(),
        }
    }

    fn channel_ref(&self) -> Option<ChannelRef> {
        self.id.as_ref().map(|id| ChannelRef { login: self.login.clone(), id: id.clone() })
    }
}

pub struct Hub {
    config: Config,
    channels: Vec<Channel>,
    auth: AuthState,
    avatar: Option<String>,
    global_badges: Arc<BadgeMap>,
    global_emotes: usize,
    events: broadcast::Sender<Arc<str>>,
    backend: Option<mpsc::UnboundedSender<Command>>,
    store: Option<Arc<EmoteStore>>,
    demo: Option<EmoteMap>,
    demo_counter: u64,
    jarvis: Option<JarvisHandle>,
    jarvis_status: Arc<std::sync::Mutex<JarvisStatus>>,
    updater: Option<UpdaterHandle>,
    /// Wersja, o którą LimboAI już zapytał (pyta raz o każdą).
    announced_update: Option<String>,
    /// Streamer zgodził się na instalację — przy najbliższej ciszy.
    install_requested: bool,
    helix: Option<Arc<Helix>>,
    ui_tx: mpsc::UnboundedSender<UiEvent>,
    me: HubHandle,
    eventsub: Option<tokio::task::JoinHandle<()>>,
    /// Gift-bomby w toku: (kanał, darczyńca, ile jeszcze pojedynczych gifów zwinąć, od kiedy, id alertu).
    gift_waves: Vec<(String, String, u64, Instant, String)>,
    memory: Arc<Memory>,
    obs: ObsHandle,
    /// Statystyki trwającego streama.
    stats: Option<Stats>,
    last_summary: Option<Summary>,
    /// Czy OBS zgłosił, że stream trwa (wtedy koniec streama wykrywa OBS, a nie Twitch).
    obs_live: bool,
    /// Ostatni znany stan z Twitcha (do wykrycia końca streama bez OBS).
    twitch_live: Option<bool>,
    /// Czat do nauki pamięci: (login, nick, tekst).
    learn_buffer: Vec<(String, String, String)>,
    /// Kiedy ostatnio zakończył się stream (Twitch jeszcze chwilę pokazuje „na żywo”).
    stream_ended: Option<Instant>,
    /// Ostatnie powitanie danego VIP-a (żeby nie witać przy każdym „siema”).
    greeted: std::collections::HashMap<String, Instant>,
    demo_alert: usize,
    studio: crate::studio::Store,
    quiz: Option<crate::games::Quiz>,
    boss: Option<crate::games::Boss>,
    games_dirty: bool,
    ballot_overlay: Option<serde_json::Value>,
    studio_busy: std::collections::HashSet<&'static str>,
    vision_frame: Option<(String,String,i64)>,
    vision_generation:u64,
    replay_stop: Option<watch::Sender<bool>>,
}

impl Hub {
    pub fn spawn(rt: &Handle, mut shutdown: watch::Receiver<bool>, link: EngineLink) -> HubHandle {
        let (requests, mut request_rx) = mpsc::unbounded_channel();
        let (ui_tx, mut ui_rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(2048);

        let demo_mode = demo::enabled();
        let (backend, store, helix) = if demo_mode {
            (None, None, None)
        } else {
            // Panel jest webowy — „budzenie” UI nie jest potrzebne.
            let ui = UiSender::new(ui_tx.clone(), Arc::new(|| {}));
            let b = backend::spawn(rt, ui, BackendConfig::default());
            (Some(b.commands), Some(b.emotes), Some(b.helix))
        };
        let config = config::load();
        let jarvis_status = Arc::new(std::sync::Mutex::new(JarvisStatus::default()));
        let memory = Arc::new(Memory::load());
        let (obs_tx, mut obs_rx) = mpsc::unbounded_channel();
        let obs = {
            let _guard = rt.enter();
            obs::spawn(config.obs.clone(), obs_tx)
        };
        let jarvis = jarvis::spawn(
            rt,
            helix.clone(),
            events.clone(),
            HubHandle { requests: requests.clone() },
            config.jarvis.clone(),
            jarvis_status.clone(),
            demo_mode,
            Some(obs.clone()),
            Some(memory.clone()),
        );

        let mut hub = Hub {
            config,
            channels: Vec::new(),
            auth: AuthState::Restoring,
            avatar: None,
            global_badges: Arc::default(),
            global_emotes: 0,
            events,
            backend,
            store,
            demo: demo_mode.then(demo::emote_map),
            demo_counter: 0,
            jarvis: Some(jarvis),
            jarvis_status,
            updater: link.updater,
            announced_update: None,
            install_requested: false,
            helix,
            ui_tx: ui_tx.clone(),
            me: HubHandle { requests: requests.clone() },
            eventsub: None,
            gift_waves: Vec::new(),
            greeted: std::collections::HashMap::new(),
            memory,
            obs,
            stats: None,
            last_summary: None,
            obs_live: false,
            twitch_live: None,
            learn_buffer: Vec::new(),
            stream_ended: None,
            demo_alert: 0,
            studio:crate::studio::Store::load(demo_mode),quiz:None,boss:None,games_dirty:false,ballot_overlay:None,studio_busy:Default::default(),vision_frame:None,vision_generation:0,replay_stop:None,
        };

        if hub.demo.is_some() {
            hub.setup_demo();
        } else {
            for login in hub.config.channels.clone() {
                hub.channels.push(Channel::new(login.clone()));
                hub.command(Command::Join(login));
            }
        }

        // Czat przejęty od poprzedniej wersji silnika (aktualizacja bez restartu OBS).
        let mut carried = link.handover.map(|h| h.channels).unwrap_or_default();
        for ch in &mut hub.channels {
            if let Some(i) = carried.iter().position(|(l, _)| *l == ch.login) {
                ch.carried = carried.swap_remove(i).1.into_iter().rev().take(HISTORY).rev().collect();
                if hub.demo.is_some() {
                    // Demo: bez ponownej przykładowej historii (byłaby zdublowana).
                    ch.history.clear();
                    hub.demo_counter = 10_000;
                }
            }
        }

        rt.spawn(async move {
            let _keep_sender = ui_tx;
            let mut demo_tick = tokio::time::interval(Duration::from_millis(1400));
            let mut minute = tokio::time::interval(Duration::from_secs(60));
            minute.tick().await;
            let mut minutes = 0u64;
            let mut game_tick=tokio::time::interval(Duration::from_millis(500));
            if hub.demo.is_some() {
                // Demo: „stream” trwa od startu, żeby dało się obejrzeć podsumowanie.
                hub.stats = Some(crate::demo::stream_stats(chrono::Utc::now() - chrono::Duration::minutes(97)));
            }
            loop {
                tokio::select! {
                    _ = game_tick.tick()=>hub.tick_games(),
                    Some(event) = ui_rx.recv() => hub.on_event(event),
                    Some(request) = request_rx.recv() => hub.on_request(request),
                    Some(event) = obs_rx.recv() => hub.on_obs(event),
                    _ = demo_tick.tick(), if hub.demo.is_some() => hub.demo_message(),
                    _ = minute.tick() => {
                        minutes += 1;
                        hub.memory.save_if_dirty();
                        hub.studio.save();
                        if minutes % 2 == 0 {
                            hub.poll_twitch_live();
                        }
                        if minutes % 10 == 0 {
                            hub.learn_from_chat();
                        }
                    }
                    _ = shutdown.changed() => break,
                }
            }
            hub.memory.save_if_dirty();
            hub.studio.save();
            if let Some(stop)=hub.replay_stop.take(){let _=stop.send(true);}
            if let Some(task) = hub.eventsub.take() {
                task.abort();
            }
            hub.command(Command::Shutdown);
        });

        HubHandle { requests }
    }

    fn command(&self, cmd: Command) {
        if let Some(backend) = &self.backend {
            let _ = backend.send(cmd);
        }
    }

    fn broadcast(&self, msg: &ServerMsg) {
        if let Ok(json) = serde_json::to_string(msg) {
            // Brak odbiorców (panel zamknięty) to nie błąd.
            let _ = self.events.send(json.into());
        }
    }

    fn reply(conn: &Outbox, msg: &ServerMsg) {
        if let Ok(json) = serde_json::to_string(msg) {
            let _ = conn.send(json.into());
        }
    }

    fn notice(&self, channel: Option<String>, text: impl Into<String>, error: bool) {
        self.broadcast(&ServerMsg::Notice { channel, text: text.into(), error });
    }

    fn channel_mut(&mut self, login: &str) -> Option<&mut Channel> {
        self.channels.iter_mut().find(|c| c.login == login)
    }

    fn emote_map(&self, channel_id: Option<&str>) -> EmoteMap {
        match (&self.demo, &self.store) {
            (Some(map), _) => map.clone(),
            (None, Some(store)) => store.map_for(channel_id.unwrap_or_default()),
            (None, None) => EmoteMap::default(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            auth: auth_dto(&self.auth),
            avatar: self.avatar.clone(),
            settings: self.config.clone(),
            global_badges: badges_dto(&self.global_badges),
            global_emotes: self.global_emotes,
            channels: self.channels.iter().map(channel_dto).collect(),
            demo: self.demo.is_some(),
            jarvis: self.jarvis_status.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            version: crate::version().to_owned(),
            update: self.updater.as_ref().map(UpdaterHandle::status).unwrap_or_default(),
            obs: self.obs.status(),
            changelog: self
                .updater
                .as_ref()
                .and_then(|u| u.status().updated_from)
                .map(|from| changelog::since(Some(&from)))
                .unwrap_or_default(),
            summary: self.last_summary.clone(),
            memory_count: self.memory.len(),
            studio:self.studio_state(),
        }
    }

    /// Kontekst dla Jarvisa: kto jest zalogowany, który kanał jest otwarty, kto pisze.
    fn jarvis_context(&self) -> jarvis::tools::Context {
        let me = match &self.auth {
            AuthState::LoggedIn { login, user_id } => Some((login.clone(), user_id.clone())),
            _ => None,
        };
        let active = self
            .config
            .active
            .as_ref()
            .and_then(|login| self.channels.iter().find(|c| &c.login == login))
            .or_else(|| self.channels.first());
        let mut chatters = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut recent = Vec::new();
        if let Some(ch) = active {
            for m in ch.history.iter().rev() {
                if seen.insert(m.sender_login.clone()) && chatters.len() < 80 {
                    chatters.push(jarvis::tools::Chatter { name: m.sender_name.clone(), login: m.sender_login.clone(), id: m.sender_id.clone() });
                }
            }
            recent = ch.history.iter().rev().take(50).rev().map(|m| (m.sender_name.clone(), m.raw_text.clone())).collect();
        }
        jarvis::tools::Context {
            me,
            active: active.and_then(|c| c.channel_ref()),
            can_moderate_active: active.is_some_and(|c| c.can_moderate),
            memory: if self.config.memory.enabled {
                chatters.iter().take(25).filter_map(|c| Some((c.name.clone(), self.memory.brief(&c.login)?))).collect()
            } else {
                Vec::new()
            },
            recent,
            demo: self.demo.is_some(),
            obs: self.config.obs.enabled.then(|| self.obs.status()),
            chatters,
            update: self.ready_update(),
        }
    }

    /// Pobrana nowa wersja czekająca na zgodę: (numer, zmiany).
    fn ready_update(&self) -> Option<(String, String)> {
        let status = self.updater.as_ref()?.status();
        (status.state == "ready").then(|| (status.available.clone().unwrap_or_default(), status.notes.clone().unwrap_or_default()))
    }

    /// W trybie „pytaj” LimboAI raz ogłasza każdą pobraną wersję.
    fn maybe_announce_update(&mut self, status: &UpdateStatus) {
        let Some(version) = status.available.clone().filter(|_| status.state == "ready") else { return };
        if self.config.updates != "ask" || self.announced_update.as_deref() == Some(version.as_str()) {
            return;
        }
        self.announced_update = Some(version.clone());
        if let Some(j) = &self.jarvis {
            j.announce_update(version);
        }
    }

    fn save_config(&mut self) {
        self.config.channels = self.channels.iter().map(|c| c.login.clone()).collect();
        if self.demo.is_none() {
            config::save(&self.config);
        }
    }

    // ── Zdarzenia silnika ──────────────────────────────────────────────

    fn on_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Connected { channel, channel_id } => {
                if let Some(ch) = self.channel_mut(&channel) {
                    ch.id = Some(channel_id);
                    self.broadcast(&ServerMsg::Connected { channel });
                }
            }
            UiEvent::Message(msg) => self.push_message(*msg),
            UiEvent::MessageDeleted { channel, message_id } => {
                if let Some((login,text))=self.channels.iter().find(|c|c.login==channel).and_then(|c|c.history.iter().find(|m|m.id==message_id)).map(|m|(m.sender_login.clone(),m.raw_text.clone())){self.studio.moderation(&channel,&login,"delete",&text);}
                if let Some(ch) = self.channel_mut(&channel) {
                    if let Some(m) = ch.history.iter_mut().rev().find(|m| m.id == message_id) {
                        m.deleted = true;
                    }
                    if let Some(m) = ch.carried.iter_mut().chain(ch.alerts.iter_mut()).find(|m| m.id == message_id) {
                        m.deleted = true;
                    }
                }
                self.broadcast(&ServerMsg::Deleted { channel, id: message_id });
            }
            UiEvent::UserCleared { channel, user_id, login, timeout_secs } => {
                self.studio.moderation(&channel,&login,if timeout_secs.is_some(){"timeout"}else{"ban"},&timeout_secs.map(|s|format!("{s} sekund")).unwrap_or_default());
                if let Some(ch) = self.channel_mut(&channel) {
                    for m in ch.history.iter_mut().filter(|m| m.sender_id == user_id) {
                        m.deleted = true;
                    }
                    for m in ch.carried.iter_mut().chain(ch.alerts.iter_mut()).filter(|m| m.user_id == user_id) {
                        m.deleted = true;
                    }
                }
                self.broadcast(&ServerMsg::UserCleared { channel, user_id, login, timeout: timeout_secs });
            }
            UiEvent::ChatCleared { channel } => {
                if let Some(ch) = self.channel_mut(&channel) {
                    for m in &mut ch.history {
                        m.deleted = true;
                    }
                    for m in ch.carried.iter_mut().chain(ch.alerts.iter_mut()) {
                        m.deleted = true;
                    }
                }
                self.broadcast(&ServerMsg::ChatCleared { channel });
            }
            UiEvent::EmotesLoaded { channel, count } => self.on_emotes_loaded(channel, count),
            UiEvent::Auth(state) => {
                if matches!(state, AuthState::LoggedOut) {
                    self.avatar = None;
                    for ch in &mut self.channels {
                        ch.can_moderate = false;
                    }
                }
                self.auth = state;
                self.broadcast(&ServerMsg::Auth(auth_dto(&self.auth)));
                self.restart_eventsub();
            }
            UiEvent::Avatar { url } => {
                self.avatar = Some(url.clone());
                self.broadcast(&ServerMsg::Avatar { url });
            }
            UiEvent::ModStatus { channel, can_moderate } => {
                if let Some(ch) = self.channel_mut(&channel) {
                    ch.can_moderate = can_moderate;
                }
                self.broadcast(&ServerMsg::ModStatus { channel, can_moderate });
            }
            UiEvent::Badges { channel, map } => {
                let dto = badges_dto(&map);
                match &channel {
                    None => self.global_badges = map,
                    Some(login) => {
                        if let Some(ch) = self.channel_mut(login) {
                            ch.badges = map;
                        }
                    }
                }
                self.broadcast(&ServerMsg::Badges { channel, map: dto });
            }
            UiEvent::Notice { channel, text } => self.notice(channel, text, false),
            UiEvent::Unbanned{channel,login}=>self.studio.moderation(&channel,&login,"unban","Potwierdzone przez Twitch"),
            UiEvent::Alert(alert) => self.on_alert(*alert, false),
            UiEvent::Error { channel, text } => self.notice(channel, text, true),
        }
    }

    fn push_message(&mut self, msg: ParsedMessage) {
        self.studio_chat(&msg);
        let dto = MessageDto::from(&msg);
        if let (AuthState::LoggedIn { login, .. }, Some(j)) = (&self.auth, &self.jarvis) {
            if self.config.jarvis.read_mentions && mentions(&msg.raw_text, &msg.sender_login, login) {
                j.mention(msg.sender_name.clone(), msg.raw_text.clone());
            }
        }
        // Kandydat na powitanie VIP-a (sprawdzane po wyświetleniu wiadomości).
        let login = msg.sender_login.to_lowercase();
        let vip = ((login == CREATOR && self.config.greetings.creator) || self.config.greetings.custom.iter().any(|(n, _)| *n == login))
            .then(|| msg.clone());
        self.track_message(&msg);
        let Some(ch) = self.channel_mut(&msg.channel_login) else { return };
        ch.history.push_back(msg);
        if ch.history.len() > HISTORY {
            ch.history.pop_front();
        }
        while ch.history.len() + ch.carried.len() > HISTORY && ch.carried.pop_front().is_some() {}
        self.broadcast(&ServerMsg::Message { m: dto });
        if let Some(msg) = vip {
            self.vip_greeting(&msg, false);
        }
    }

    /// Po załadowaniu emotek 7TV uzupełniamy je w historii i odsyłamy ją
    /// panelom (wiadomości sprzed odpowiedzi 7TV dostają obrazki).
    fn on_emotes_loaded(&mut self, channel: Option<String>, count: usize) {
        match &channel {
            None => self.global_emotes = count,
            Some(login) => {
                if let Some(ch) = self.channel_mut(login) {
                    ch.emote_count = count;
                }
            }
        }
        let targets: Vec<usize> = self
            .channels
            .iter()
            .enumerate()
            .filter(|(_, c)| channel.as_deref().is_none_or(|l| l == c.login))
            .map(|(i, _)| i)
            .collect();
        for i in targets {
            let map = self.emote_map(self.channels[i].id.as_deref());
            let ch = &mut self.channels[i];
            let mut changed = false;
            for m in &mut ch.history {
                changed |= parser::retokenize(&mut m.segments, &map, EmoteScale::X1);
            }
            if changed {
                let dto = channel_dto(ch);
                self.broadcast(&ServerMsg::History { channel: dto.login.clone(), messages: dto.messages });
            }
        }
        self.broadcast(&ServerMsg::EmotesChanged { channel, count });
    }

    // ── Komendy panelu ─────────────────────────────────────────────────

    fn on_request(&mut self, request: Request) {
        match request {
            Request::StudioCommand{command,reply}=>{let result=self.studio_command(command,None);let _=reply.send(result);}
            Request::StudioDone(result)=>self.studio_done(result),
            Request::Connect { reply } => {
                let snapshot = serde_json::to_string(&ServerMsg::Snapshot(self.snapshot())).unwrap_or_default();
                let _ = reply.send((snapshot.into(), self.events.subscribe()));
            }
            Request::Client { msg, conn } => self.on_client(msg, &conn),
            Request::Export { reply } => {
                let channels = self
                    .channels
                    .iter()
                    .map(|c| (c.login.clone(), channel_dto(c).messages))
                    .collect();
                let _ = reply.send(Handover { channels });
            }
            Request::UpdateGate { reply } => {
                let auto = self.config.updates != "ask" || self.install_requested;
                let idle = self.jarvis.as_ref().is_none_or(JarvisHandle::is_idle)
                    && self.quiz.is_none() && self.boss.is_none() && self.studio_busy.is_empty();
                let _ = reply.send((auto, idle));
            }
            Request::UpdateStatus(status) => {
                self.broadcast(&ServerMsg::Update(status.clone()));
                self.maybe_announce_update(&status);
            }
            Request::InstallUpdate => self.install_requested = true,
            Request::AlertRhyme { channel, id, rhyme } => self.on_rhyme(&channel, &id, rhyme),
            Request::ClipMade(url) => {
                if let Some(st) = &mut self.stats {
                    st.on_clip(&url);
                }
            }
            Request::SummaryDone { id, text, title, category } => {
                let Some(sum) = self.last_summary.as_mut().filter(|s| s.id == id) else { return };
                sum.ai_text = text;
                sum.ai_pending = false;
                sum.title = sum.title.take().or(title);
                sum.category = sum.category.take().or(category);
                let sum = sum.clone();
                summary::save(&sum);
                self.broadcast(&ServerMsg::Summary(sum));
            }
            Request::MemoryFacts(facts) => {
                let mut added = 0;
                for (login, fact) in facts {
                    let name = self.memory.get(&login).map(|v| v.name).filter(|n| !n.is_empty()).unwrap_or_else(|| login.clone());
                    if self.memory.remember(&login, &name, &fact, "ai") {
                        added += 1;
                    }
                }
                if added > 0 {
                    tracing::info!("pamięć: LimboAI zapamiętał {added} nowych faktów z czatu");
                }
            }
            Request::TwitchLive { viewers, started_at } => self.on_twitch_live(viewers, started_at),
            Request::JarvisContext { reply } => {
                let _ = reply.send(self.jarvis_context());
            }
            Request::SetAccent { accent } => self.set_accent(&accent),
            Request::OpenChannel { login, reply } => {
                let _ = reply.send(self.open_channel(&login));
            }
        }
    }

    fn on_client(&mut self, msg: ClientMsg, conn: &Outbox) {
        match msg {
            ClientMsg::Studio{command}=>{if let Err(text)=self.studio_command(command,Some(conn)){Self::reply(conn,&ServerMsg::Notice{channel:None,text,error:true});}}
            ClientMsg::Join { channel } => {
                if let Err(text) = self.open_channel(&channel) {
                    Self::reply(conn, &ServerMsg::Notice { channel: None, text, error: true });
                }
            }
            ClientMsg::Theme { accent } => self.set_accent(&accent),
            ClientMsg::Part { channel } => {
                self.channels.retain(|c| c.login != channel);
                self.command(Command::Part(channel.clone()));
                self.save_config();
                self.broadcast(&ServerMsg::ChannelRemoved { channel });
            }
            ClientMsg::Activate { channel } => {
                self.config.active = Some(channel);
                self.save_config();
            }
            ClientMsg::Login => self.command(Command::Login),
            ClientMsg::CancelLogin => self.command(Command::CancelLogin),
            ClientMsg::Logout => self.command(Command::Logout),
            ClientMsg::Send { channel, text } => {
                if self.demo.is_some() {
                    let map = self.emote_map(None);
                    self.push_message(demo::echo(&channel, &text, &map));
                    return;
                }
                match self.channel_mut(&channel).and_then(|c| c.channel_ref()) {
                    Some(channel) => self.command(Command::SendMessage { channel, text }),
                    None => Self::reply(conn, &ServerMsg::Notice { channel: Some(channel), text: "Trwa łączenie z kanałem…".into(), error: true }),
                }
            }
            ClientMsg::Timeout { channel, user_id, login, seconds } => {
                self.moderate(&channel, |c| Command::Timeout { channel: c, user_id: user_id.clone(), login: login.clone(), seconds, reason: None });
                self.demo_moderation(&channel, Some(&user_id), &login, Some(seconds as u64), None);
            }
            ClientMsg::Ban { channel, user_id, login } => {
                self.moderate(&channel, |c| Command::Ban { channel: c, user_id: user_id.clone(), login: login.clone(), reason: None });
                self.demo_moderation(&channel, Some(&user_id), &login, None, None);
            }
            ClientMsg::Unban { channel, user_id, login } => {
                self.moderate(&channel, |c| Command::Unban { channel: c, user_id: user_id.clone(), login: login.clone() });
            }
            ClientMsg::Delete { channel, message_id } => {
                self.moderate(&channel, |c| Command::DeleteMessage { channel: c, message_id: message_id.clone() });
                self.demo_moderation(&channel, None, "", None, Some(&message_id));
            }
            ClientMsg::Emotes { channel } => {
                let id = self.channels.iter().find(|c| c.login == channel).and_then(|c| c.id.clone());
                let emotes: Vec<EmoteDto> =
                    self.emote_map(id.as_deref()).sorted_for_picker().iter().map(|e| EmoteDto::from(e.as_ref())).collect();
                Self::reply(conn, &ServerMsg::EmoteList { channel, emotes });
            }
            ClientMsg::Open { url } => {
                if url.starts_with("https://") || url.starts_with("http://") {
                    if let Err(e) = open::that_detached(&url) {
                        tracing::warn!("nie udało się otworzyć adresu: {e}");
                    }
                }
            }
            ClientMsg::JarvisActivate => {
                if let Some(j) = &self.jarvis {
                    j.activate();
                }
            }
            ClientMsg::JarvisRefresh => {
                if let Some(j) = &self.jarvis {
                    j.refresh();
                }
            }
            ClientMsg::JarvisKey { provider, key } => {
                match (jarvis::secrets::Provider::parse(&provider), &self.jarvis) {
                    (Some(p), Some(j)) => j.set_key(p, key.filter(|k| !k.trim().is_empty())),
                    _ => Self::reply(conn, &ServerMsg::Notice { channel: None, text: "Nieznany dostawca klucza.".into(), error: true }),
                }
            }
            ClientMsg::JarvisEnroll { action } => {
                if let Some(j) = &self.jarvis {
                    j.enroll(&action);
                }
            }
            ClientMsg::JarvisSettings(mut c) => {
                c.voice_volume=c.voice_volume.min(100);
                if jarvis::hotkey::parse_combo(&c.hotkey).is_none() {
                    Self::reply(conn, &ServerMsg::Notice {
                        channel: None,
                        text: "Skrót: jeden klawisz F1–F24 / Num / boczny przycisk myszy albo 2–4 klawisze z co najmniej jednym zwykłym.".into(),
                        error: true,
                    });
                    return;
                }
                if !jarvis::MODELS.iter().any(|(id, _)| *id == c.model) {
                    c.model = jarvis::MODELS[0].0.to_owned();
                }
                c.mic = c.mic.filter(|m| !m.is_empty());
                c.wake_sensitivity = c.wake_sensitivity.clamp(1, 3);
                if c.brain != "custom" {
                    c.brain = "gemini".into();
                }
                if jarvis::openai::preset(&c.llm_preset).is_none() {
                    c.llm_preset = "openai".into();
                }
                if c.talk_mode != "hold" {
                    c.talk_mode = "toggle".into();
                }
                c.llm_url = c.llm_url.trim().chars().take(300).collect();
                c.llm_model = c.llm_model.trim().chars().take(200).collect();
                self.config.jarvis = c;
                self.save_config();
                if let Some(j) = &self.jarvis {
                    j.set_config(self.config.jarvis.clone());
                }
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
            ClientMsg::Alerts(alerts) => {
                let eventsub_changed = alerts.follows != self.config.alerts.follows || alerts.redemptions != self.config.alerts.redemptions;
                self.config.alerts = alerts;
                self.save_config();
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
                if eventsub_changed {
                    self.restart_eventsub();
                }
            }
            ClientMsg::MediaPreviews { mode } => {
                self.config.media_previews = match mode.as_str() {
                    "trusted" | "off" => mode,
                    _ => "all".into(),
                };
                self.save_config();
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
            ClientMsg::ObsSettings(c) => {
                self.config.obs = c.clone();
                self.save_config();
                self.obs.configure(c);
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
            ClientMsg::ObsPassword { password } => {
                let obs = self.obs.clone();
                tokio::spawn(async move {
                    let password = password.filter(|p| !p.trim().is_empty());
                    if let Err(e) = jarvis::secrets::store(jarvis::secrets::Provider::Obs, password).await {
                        tracing::warn!("nie udało się zapisać hasła OBS: {e:#}");
                    }
                    obs.reconnect();
                });
            }
            ClientMsg::MemorySettings(c) => {
                self.config.memory = c;
                self.save_config();
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
            ClientMsg::MemoryGet { login } => self.reply_memory(conn, &login),
            ClientMsg::MemoryAdd { login, name, text } => {
                self.memory.remember(&login, &name, &text, "panel");
                self.reply_memory(conn, &login);
            }
            ClientMsg::MemoryForget { login, index } => {
                self.memory.forget(&login, Forget::Index(index));
                self.reply_memory(conn, &login);
            }
            ClientMsg::MemoryClear => {
                self.memory.clear();
                self.memory.save_if_dirty();
                self.reply_memory(conn, "");
            }
            ClientMsg::SummarySettings(c) => {
                self.config.summary = c;
                self.save_config();
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
            ClientMsg::SummaryNow => self.finish_stream(false, true),
            ClientMsg::SummaryHistory => Self::reply(conn, &ServerMsg::Summaries { list: summary::history() }),
            ClientMsg::Changelog => Self::reply(conn, &ServerMsg::Changelog { entries: changelog::recent() }),
            ClientMsg::Greetings(mut g) => {
                g.custom = g
                    .custom
                    .into_iter()
                    .map(|(n, t)| (n.trim().trim_start_matches('@').to_lowercase(), t.trim().chars().take(200).collect::<String>()))
                    .filter(|(n, t)| !n.is_empty() && !t.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                    .take(50)
                    .collect();
                self.config.greetings = g;
                self.save_config();
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
            ClientMsg::AlertTest { kind } if kind == "vip" => {
                let channel = self.config.active.clone().or_else(|| self.channels.first().map(|c| c.login.clone()));
                if let Some(channel) = channel {
                    let map = self.emote_map(None);
                    let mut msg = demo::echo(&channel, "siema wszystkim o/", &map);
                    msg.sender_login = CREATOR.into();
                    msg.sender_name = "kosmiaczek".into();
                    msg.id = format!("vip-test-{}", chrono::Utc::now().timestamp_millis());
                    self.vip_greeting(&msg, true);
                }
            }
            ClientMsg::AlertTest { kind } => {
                let channel = self
                    .config
                    .active
                    .clone()
                    .filter(|a| self.channels.iter().any(|c| &c.login == a))
                    .or_else(|| self.channels.first().map(|c| c.login.clone()));
                match channel {
                    Some(channel) => {
                        let map = self.emote_map(None);
                        self.on_alert(demo::alert(&channel, &kind, &map), true);
                    }
                    None => Self::reply(conn, &ServerMsg::Notice { channel: None, text: "Otwórz najpierw jakiś kanał.".into(), error: false }),
                }
            }
            ClientMsg::UpdateCheck => {
                if let Some(u) = &self.updater {
                    u.check();
                }
            }
            ClientMsg::UpdateApply => {
                if let Some(u) = &self.updater {
                    u.apply();
                }
            }
            ClientMsg::UpdateMode { mode } => {
                self.config.updates = if mode == "ask" { "ask".into() } else { "auto".into() };
                self.save_config();
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
            ClientMsg::JarvisModels => {
                if let Some(j) = &self.jarvis {
                    j.list_models();
                }
            }
            ClientMsg::Overlay(o) => {
                self.config.overlay = o.sanitized();
                self.save_config();
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
            ClientMsg::Settings { animations, timestamps, mod_buttons } => {
                self.config.animations = animations;
                self.config.timestamps = timestamps;
                self.config.mod_buttons = mod_buttons;
                self.save_config();
                self.broadcast(&ServerMsg::Settings(self.config.clone()));
            }
        }
    }

    /// Otwiera kanał (lub przełącza na już otwarty) we wszystkich panelach.
    fn open_channel(&mut self, name: &str) -> Result<String, String> {
        let login = name.trim().trim_start_matches(['#', '@']).to_lowercase();
        let valid = (1..=25).contains(&login.len()) && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!("„{login}” to niepoprawna nazwa kanału"));
        }
        if self.channel_mut(&login).is_none() {
            let mut ch = Channel::new(login.clone());
            ch.id = self.demo.as_ref().map(|_| "0".to_owned());
            self.channels.push(ch);
            self.command(Command::Join(login.clone()));
        }
        self.config.active = Some(login.clone());
        self.save_config();
        self.broadcast(&ServerMsg::ChannelAdded { channel: login.clone() });
        Ok(login)
    }

    fn set_accent(&mut self, accent: &str) {
        let Some(accent) = config::parse_accent(accent) else { return };
        self.config.accent = accent;
        self.save_config();
        self.broadcast(&ServerMsg::Settings(self.config.clone()));
    }

    fn moderate(&mut self, channel: &str, build: impl FnOnce(ChannelRef) -> Command) {
        if self.demo.is_some() {
            return;
        }
        match self.channel_mut(channel).and_then(|c| c.channel_ref()) {
            Some(r) => self.command(build(r)),
            None => self.notice(Some(channel.to_owned()), "Trwa łączenie z kanałem…", true),
        }
    }

    // ── Tryb demo ──────────────────────────────────────────────────────

    fn setup_demo(&mut self) {
        self.auth = AuthState::LoggedIn { login: "ty".into(), user_id: "0".into() };
        let map = self.emote_map(None);
        for (login, count) in [("demo", 14usize), ("drugi_kanal", 4)] {
            let mut ch = Channel::new(login.to_owned());
            ch.id = Some("0".into());
            ch.can_moderate = true;
            ch.emote_count = 6;
            for i in 0..count {
                ch.history.push_back(demo::message(login, i as u64, &map));
            }
            self.channels.push(ch);
        }
        self.global_emotes = 6;
        self.config.active = Some("demo".into());
    }

    // ── Alerty ─────────────────────────────────────────────────────────

    fn on_alert(&mut self, alert: ChatAlert, test: bool) {
        let settings = self.config.alerts.clone();
        match &alert.kind {
            AlertKind::NewChatter if !settings.new_chatters => {
                // Bez karty — zwykła wiadomość „pierwszy raz na czacie”.
                if let Some(m) = alert.message {
                    self.push_message(m);
                }
                return;
            }
            AlertKind::Follow if !settings.follows && !test => return,
            AlertKind::Redemption { .. } if !settings.redemptions && !test => return,
            _ => {}
        }
        let sender = if matches!(alert.kind, AlertKind::GiftSub { anonymous: true, .. } | AlertKind::MassGift { anonymous: true, .. }) {
            String::new()
        } else {
            alert.user_login.clone()
        };
        let now = Instant::now();
        self.gift_waves.retain(|w| now.duration_since(w.3) < Duration::from_secs(90) && w.2 > 0);
        if !test {
            let in_wave = matches!(alert.kind, AlertKind::GiftSub { .. })
                && self.gift_waves.iter().any(|w| w.0 == alert.channel_login && w.1 == sender && w.2 > 0);
            self.track_alert(&alert, in_wave);
        }

        // Pojedyncze gifty z gift-bomby dopisujemy do karty bomby zamiast nowych kart.
        if let AlertKind::GiftSub { recipient, .. } = &alert.kind {
            if let Some(i) = self.gift_waves.iter().position(|w| w.0 == alert.channel_login && w.1 == sender) {
                self.gift_waves[i].2 -= 1;
                let (channel, id) = (self.gift_waves[i].0.clone(), self.gift_waves[i].4.clone());
                if let Some(entry) = self.channel_mut(&channel).and_then(|c| c.alerts.iter_mut().find(|m| m.id == id)) {
                    if let Some(dto) = &mut entry.alert {
                        let mut names: Vec<String> = dto.extra.as_deref().unwrap_or_default().split(", ").filter(|s| !s.is_empty()).map(str::to_owned).collect();
                        if names.len() < 12 {
                            names.push(recipient.clone());
                        }
                        dto.extra = Some(names.join(", "));
                        let update = ServerMsg::AlertUpdate { channel: channel.clone(), id: id.clone(), alert: dto.clone() };
                        self.broadcast(&update);
                    }
                }
                return;
            }
        }

        // Kanał: ten z alertu, a jeśli nie jest otwarty (np. follow na Twoim kanale) — aktywny.
        let channel = if self.channels.iter().any(|c| c.login == alert.channel_login) {
            alert.channel_login.clone()
        } else {
            match self.config.active.clone().filter(|a| self.channels.iter().any(|c| &c.login == a)).or_else(|| self.channels.first().map(|c| c.login.clone())) {
                Some(c) => c,
                None => return,
            }
        };

        let eligible = rhyme_event(&alert).is_some();
        let mut dto = alert_dto(&alert);
        dto.test = test;
        dto.rhyme_pending = settings.rhymes && eligible && self.jarvis.is_some();
        let mut message = match &alert.message {
            Some(m) => MessageDto::from(m),
            None => MessageDto {
                id: alert.id.clone(),
                channel: channel.clone(),
                user_id: alert.user_id.clone(),
                login: alert.user_login.clone(),
                name: alert.user_name.clone(),
                color: alert.color.map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}")),
                badges: Vec::new(),
                action: false,
                ts: alert.timestamp.timestamp_millis(),
                deleted: false,
                raw: String::new(),
                segs: Vec::new(),
                alert: None,
            },
        };
        message.id = alert.id.clone();
        message.channel = channel.clone();
        message.alert = Some(dto.clone());
        if let AlertKind::MassGift { count, .. } = &alert.kind {
            self.gift_waves.push((alert.channel_login.clone(), sender, *count, now, alert.id.clone()));
        }

        let Some(ch) = self.channel_mut(&channel) else { return };
        ch.alerts.push_back(message.clone());
        while ch.alerts.len() > 40 {
            ch.alerts.pop_front();
        }
        let channel_ref = ch.channel_ref();
        self.broadcast(&ServerMsg::Message { m: message });

        if dto.rhyme_pending {
            if let (Some(jarvis), Some(event)) = (&self.jarvis, rhyme_event(&alert)) {
                // Pamięć o widzu → rymowanka „o nim”.
                let event = match self.config.memory.enabled.then(|| self.memory.brief(&alert.user_login)).flatten() {
                    Some(brief) => format!("{event}\nCo LimboAI wie o widzu (możesz nawiązać): {brief}"),
                    None => event,
                };
                let reply = jarvis.rhyme(alert.user_name.clone(), event);
                let hub = self.me.clone();
                let id = alert.id.clone();
                tokio::spawn(async move {
                    let rhyme = reply.await.ok().flatten();
                    let _ = hub.requests.send(Request::AlertRhyme { channel, id, rhyme });
                });
            }
        }
        let _ = channel_ref;
    }

    fn on_rhyme(&mut self, channel: &str, id: &str, rhyme: Option<String>) {
        let settings = self.config.alerts.clone();
        let logged_in = matches!(self.auth, AuthState::LoggedIn { .. });
        let Some(ch) = self.channel_mut(channel) else { return };
        let channel_ref = ch.channel_ref();
        let Some(entry) = ch.alerts.iter_mut().find(|m| m.id == id) else { return };
        let login = entry.login.clone();
        let Some(dto) = &mut entry.alert else { return };
        dto.rhyme = rhyme.clone();
        dto.rhyme_pending = false;
        let test = dto.test;
        let update = ServerMsg::AlertUpdate { channel: channel.to_owned(), id: id.to_owned(), alert: dto.clone() };
        self.broadcast(&update);
        let Some(text) = rhyme else { return };
        if settings.rhymes_voice {
            if let Some(j) = &self.jarvis {
                j.say(text.clone());
            }
        }
        // Na czat Twitcha tylko prawdziwe alerty i tylko po zalogowaniu.
        if settings.rhymes_chat && !test && logged_in && self.demo.is_none() {
            if let Some(channel) = channel_ref {
                let line = format!("@{login} {}", text.replace('\n', " / "));
                self.command(Command::SendMessage { channel, text: line.chars().take(480).collect() });
            }
        }
    }

    // ── Pamięć o widzach i statystyki streama ─────────────────────────

    fn reply_memory(&self, conn: &Outbox, login: &str) {
        let viewer = if login.is_empty() { None } else { self.memory.get(login) };
        Self::reply(conn, &ServerMsg::Memory { login: login.to_owned(), viewer, count: self.memory.len() });
    }

    fn track_message(&mut self, msg: &ParsedMessage) {
        let stream_id = self.stats.as_ref().filter(|s| s.channel == msg.channel_login).map_or(0, |s| s.id);
        if self.config.memory.enabled {
            self.memory.on_message(&msg.sender_login, &msg.sender_name, stream_id);
            if self.config.memory.learn {
                self.learn_buffer.push((msg.sender_login.to_lowercase(), msg.sender_name.clone(), msg.raw_text.chars().take(200).collect()));
                if self.learn_buffer.len() > 400 {
                    self.learn_buffer.drain(..100);
                }
            }
        }
        if let Some(st) = self.stats.as_mut().filter(|s| s.channel == msg.channel_login) {
            let emotes: Vec<String> = msg
                .segments
                .iter()
                .filter_map(|s| match s {
                    parser::Segment::Emote(e) => Some(e.base.name.to_string()),
                    _ => None,
                })
                .collect();
            st.on_message(&msg.sender_login, &msg.sender_name, &msg.raw_text, &emotes, msg.timestamp);
        }
    }

    fn track_alert(&mut self, alert: &ChatAlert, in_wave: bool) {
        let (login, name) = (alert.user_login.as_str(), alert.user_name.as_str());
        if self.config.memory.enabled {
            match &alert.kind {
                AlertKind::Bits { amount } => self.memory.on_support(login, name, *amount, None, 0),
                AlertKind::Sub { months, .. } => self.memory.on_support(login, name, 0, Some(*months), 0),
                AlertKind::GiftSub { anonymous: false, .. } if !in_wave => self.memory.on_support(login, name, 0, None, 1),
                AlertKind::MassGift { count, anonymous: false, .. } => self.memory.on_support(login, name, 0, None, *count),
                _ => {}
            }
        }
        let Some(st) = self.stats.as_mut().filter(|s| s.channel == alert.channel_login) else { return };
        match &alert.kind {
            AlertKind::Bits { amount } => st.on_bits(login, name, *amount),
            AlertKind::Sub { resub, .. } => st.on_sub(login, name, *resub),
            AlertKind::GiftSub { anonymous, .. } if !in_wave => st.on_gifts(if *anonymous { "" } else { login }, name, 1),
            AlertKind::MassGift { count, anonymous, .. } => st.on_gifts(if *anonymous { "" } else { login }, name, *count),
            AlertKind::Raid { viewers, .. } => st.on_raid(name, *viewers),
            AlertKind::Follow => st.on_follow(),
            AlertKind::Redemption { .. } => st.on_redemption(),
            AlertKind::NewChatter => st.on_new_chatter(),
            _ => {}
        }
        if let Some(m) = &alert.message {
            let emotes: Vec<String> = m.segments.iter().filter_map(|s| match s {
                parser::Segment::Emote(e) => Some(e.base.name.to_string()),
                _ => None,
            }).collect();
            st.on_message(&m.sender_login, &m.sender_name, &m.raw_text, &emotes, m.timestamp);
        }
    }

    /// Co 10 minut: LimboAI wyciąga z czatu trwałe fakty o widzach.
    fn learn_from_chat(&mut self) {
        let buffer = std::mem::take(&mut self.learn_buffer);
        let (Some(jarvis), true) = (&self.jarvis, self.config.memory.enabled && self.config.memory.learn) else { return };
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (login, _, _) in &buffer {
            *counts.entry(login.as_str()).or_default() += 1;
        }
        let allowed: std::collections::HashSet<String> = counts.iter().filter(|(_, n)| **n >= 3).map(|(l, _)| (*l).to_owned()).collect();
        let lines: Vec<String> = buffer.iter().filter(|(l, _, _)| allowed.contains(l)).map(|(l, _, t)| format!("{l}: {t}")).collect();
        if lines.len() < 8 {
            return;
        }
        let prompt = lines.iter().rev().take(150).rev().cloned().collect::<Vec<_>>().join("\n");
        let reply = jarvis.ask(crate::memory::LEARN_SYSTEM, prompt);
        let hub = self.me.clone();
        tokio::spawn(async move {
            let Some(text) = reply.await.ok().flatten() else { return };
            let facts: Vec<(String, String)> = crate::memory::parse_facts(&text).into_iter().filter(|(l, _)| allowed.contains(l)).collect();
            if !facts.is_empty() {
                let _ = hub.requests.send(Request::MemoryFacts(facts));
            }
        });
    }

    fn on_obs(&mut self, event: ObsEvent) {
        match event {
            ObsEvent::Status(status) => self.broadcast(&ServerMsg::Obs(status)),
            ObsEvent::StreamStarted { seconds } => {
                self.obs_live = true;
                if self.stats.is_none() {
                    self.start_stream(chrono::Utc::now() - chrono::Duration::seconds(seconds as i64));
                }
            }
            ObsEvent::StreamStopped => {
                self.obs_live = false;
                self.finish_stream(true, false);
            }
        }
    }

    fn start_stream(&mut self, started_at: chrono::DateTime<chrono::Utc>) {
        let channel = match &self.auth {
            AuthState::LoggedIn { login, .. } => login.clone(),
            _ if self.demo.is_some() => "demo".into(),
            _ => return,
        };
        tracing::info!("stream wystartował — zbieram statystyki do podsumowania (#{channel})");
        self.stats = Some(Stats::start(&channel, started_at));
        self.stream_ended = None;
    }

    /// Co 2 minuty: widzowie z Twitcha (wykres) i — bez OBS — wykrycie końca streama.
    fn poll_twitch_live(&mut self) {
        if self.demo.is_some() {
            let n = 30 + (chrono::Utc::now().timestamp() % 60) as u64;
            if let Some(st) = &mut self.stats {
                st.on_viewers(n, chrono::Utc::now());
            }
            return;
        }
        let (AuthState::LoggedIn { user_id, .. }, Some(helix)) = (&self.auth, &self.helix) else { return };
        let (helix, id, hub) = (helix.clone(), user_id.clone(), self.me.clone());
        tokio::spawn(async move {
            let Ok(info) = helix.live_stream(&id).await else { return };
            let started_at = info.as_ref().and_then(|i| chrono::DateTime::parse_from_rfc3339(&i.started_at).ok()).map(|d| d.with_timezone(&chrono::Utc));
            let _ = hub.requests.send(Request::TwitchLive { viewers: info.map(|i| i.viewer_count), started_at });
        });
    }

    fn on_twitch_live(&mut self, viewers: Option<u64>, started_at: Option<chrono::DateTime<chrono::Utc>>) {
        match viewers {
            Some(count) => {
                let recently_ended = self.stream_ended.is_some_and(|t| t.elapsed() < Duration::from_secs(15 * 60));
                if self.stats.is_none() && !recently_ended {
                    self.start_stream(started_at.unwrap_or_else(chrono::Utc::now));
                }
                if let Some(st) = &mut self.stats {
                    st.on_viewers(count, chrono::Utc::now());
                }
                self.twitch_live = Some(true);
            }
            None => {
                // Bez OBS: Twitch pokazuje koniec streama.
                if self.twitch_live == Some(true) && !self.obs_live {
                    self.finish_stream(true, false);
                }
                self.twitch_live = Some(false);
            }
        }
    }

    /// Podsumowanie: `end` = stream się skończył (statystyki zamykamy), `manual` = przycisk w panelu.
    fn finish_stream(&mut self, end: bool, manual: bool) {
        if end && !self.config.summary.enabled {
            self.stats = None;
            self.stream_ended = Some(Instant::now());
            return;
        }
        let Some(stats) = (if end { self.stats.take() } else { self.stats.as_ref().map(|s| s.snapshot()) }) else {
            if manual {
                self.notice(None, "📊 Nie trwa żaden stream — podsumowanie pojawi się samo po kliknięciu „Zakończ stream” w OBS.", false);
            }
            return;
        };
        if end {
            self.stream_ended = Some(Instant::now());
        }
        let mut sum = stats.finish(chrono::Utc::now());
        sum.ai_pending = self.jarvis.is_some();
        self.last_summary = Some(sum.clone());
        summary::save(&sum);
        self.broadcast(&ServerMsg::Summary(sum.clone()));
        if end && self.config.summary.voice {
            if let Some(j) = &self.jarvis {
                j.say(sum.spoken());
            }
        }
        // Tytuł/kategoria i podsumowanie słowami — w tle.
        let sample = stats.sample();
        let helix = self.helix.clone();
        let own_id = match &self.auth {
            AuthState::LoggedIn { user_id, .. } => Some(user_id.clone()),
            _ => None,
        };
        let demo = self.demo.is_some();
        let jarvis = self.jarvis.clone();
        let hub = self.me.clone();
        tokio::spawn(async move {
            let (title, category) = match (helix, own_id) {
                (Some(h), Some(id)) => match h.channel_info(&id).await {
                    Ok(Some(info)) => (Some(info.title), Some(info.game_name)),
                    _ => (None, None),
                },
                _ if demo => (Some("Wieczorne granie z czatem".to_owned()), Some("Just Chatting".to_owned())),
                _ => (None, None),
            };
            let mut with_meta = sum.clone();
            with_meta.title = title.clone();
            with_meta.category = category.clone();
            let mut text = match jarvis {
                Some(j) => j.ask(summary::SUMMARY_SYSTEM, with_meta.prompt(&sample)).await.ok().flatten(),
                None => None,
            };
            if text.is_none() && demo {
                text = Some(demo_summary_text(&with_meta));
            }
            let _ = hub.requests.send(Request::SummaryDone { id: sum.id, text, title, category });
        });
    }

    /// Powitanie VIP-a: twórca LiMBOCHAT albo ktoś z własnej listy pisze „siema”, „hej”, „dzień dobry”…
    /// LimboAI mówi powitanie, na czacie pojawia się złota karta, na overlayu efekt.
    fn vip_greeting(&mut self, msg: &ParsedMessage, test: bool) -> bool {
        let cfg = self.config.greetings.clone();
        let login = msg.sender_login.to_lowercase();
        let (title, text, creator) = if login == CREATOR && cfg.creator {
            let i = (chrono::Utc::now().timestamp_millis() as usize) % CREATOR_GREETINGS.len();
            ("Szef Kosmi na czacie".to_owned(), CREATOR_GREETINGS[i].to_owned(), true)
        } else if let Some((_, text)) = cfg.custom.iter().find(|(n, _)| *n == login) {
            (format!("{} na czacie", msg.sender_name), text.clone(), false)
        } else {
            return false;
        };
        if !test && !is_greeting(&msg.raw_text) {
            return false;
        }
        // Raz na 30 minut na osobę — zwykłe „siema” co chwilę nie uruchamia efektu.
        let now = Instant::now();
        if !test && self.greeted.get(&login).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(30 * 60)) {
            return false;
        }
        self.greeted.insert(login, now);

        let alert = ChatAlert {
            id: format!("vip-{}", msg.id),
            channel_login: msg.channel_login.clone(),
            user_id: msg.sender_id.clone(),
            user_login: msg.sender_login.clone(),
            user_name: msg.sender_name.clone(),
            color: msg.color,
            kind: AlertKind::Vip { title: title.clone(), greeting: text.clone() },
            message: None,
            timestamp: msg.timestamp,
        };
        self.on_alert(alert, test);
        if cfg.voice {
            if let Some(j) = &self.jarvis {
                j.say(text.clone());
            }
        }
        if cfg.overlay {
            let ev = jarvis::JarvisEvent::Vip { name: msg.sender_name.clone(), title, text, creator };
            self.broadcast(&ServerMsg::Jarvis { ev });
        }
        true
    }

    /// (Re)start nasłuchu follow / nagród (EventSub) dla zalogowanego streamera.
    fn restart_eventsub(&mut self) {
        if let Some(task) = self.eventsub.take() {
            task.abort();
        }
        let (AuthState::LoggedIn { login, user_id }, Some(helix)) = (&self.auth, &self.helix) else { return };
        let mut types = Vec::new();
        if self.config.alerts.follows {
            types.push(eventsub::FOLLOW);
        }
        if self.config.alerts.redemptions {
            types.push(eventsub::REDEMPTION);
        }
        if types.is_empty() {
            return;
        }
        let (helix, uid) = (helix.clone(), user_id.clone());
        let subscribe: eventsub::Subscriber = Arc::new(move |kind, session| {
            let (helix, uid) = (helix.clone(), uid.clone());
            Box::pin(async move {
                let (version, condition) = if kind == eventsub::FOLLOW {
                    ("2", serde_json::json!({ "broadcaster_user_id": uid, "moderator_user_id": uid }))
                } else {
                    ("1", serde_json::json!({ "broadcaster_user_id": uid }))
                };
                let body = serde_json::json!({
                    "type": kind,
                    "version": version,
                    "condition": condition,
                    "transport": { "method": "websocket", "session_id": session },
                });
                helix
                    .request(reqwest::Method::POST, "/eventsub/subscriptions", &[], Some(&body))
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
        });
        self.eventsub = Some(eventsub::spawn(types, login.clone(), subscribe, self.ui_tx.clone()));
    }

    fn demo_message(&mut self) {
        let Some(map) = self.demo.clone() else { return };
        self.demo_counter += 1;
        let channel = if self.demo_counter % 4 == 0 { "drugi_kanal" } else { "demo" };
        self.push_message(demo::message(channel, 100 + self.demo_counter, &map));
        // Co kilka wiadomości przykładowy alert (bitsy, suby, raid…).
        if std::env::var("LIMBOCHAT_DEMO_ALERTS").map_or(true, |v| v != "0") && self.demo_counter % 7 == 3 {
            let kind = demo::ALERT_CYCLE[self.demo_alert % demo::ALERT_CYCLE.len()];
            self.demo_alert += 1;
            self.on_alert(demo::alert("demo", kind, &map), false);
        }
    }

    fn demo_moderation(&mut self, channel: &str, user_id: Option<&str>, login: &str, timeout: Option<u64>, message_id: Option<&str>) {
        if self.demo.is_none() {
            return;
        }
        match (user_id, message_id) {
            (_, Some(id)) => self.on_event(UiEvent::MessageDeleted { channel: channel.into(), message_id: id.into() }),
            (Some(uid), None) => self.on_event(UiEvent::UserCleared {
                channel: channel.into(),
                user_id: uid.into(),
                login: login.into(),
                timeout_secs: timeout,
            }),
            _ => {}
        }
    }
}

fn auth_dto(state: &AuthState) -> AuthDto {
    match state {
        AuthState::Restoring => AuthDto::Restoring,
        AuthState::LoggedOut => AuthDto::LoggedOut,
        AuthState::Pending { user_code, verification_uri, expires_at } => AuthDto::Pending {
            code: user_code.clone(),
            uri: verification_uri.clone(),
            expires_in: expires_at.saturating_duration_since(Instant::now()).as_secs(),
        },
        AuthState::LoggedIn { login, user_id } => AuthDto::LoggedIn { login: login.clone(), user_id: user_id.clone() },
    }
}

fn channel_dto(ch: &Channel) -> ChannelDto {
    ChannelDto {
        login: ch.login.clone(),
        connected: ch.id.is_some(),
        can_moderate: ch.can_moderate,
        emote_count: ch.emote_count,
        badges: badges_dto(&ch.badges),
        messages: {
            let mut all: Vec<MessageDto> = ch.carried.iter().cloned().chain(ch.history.iter().map(MessageDto::from)).collect();
            if !ch.alerts.is_empty() {
                all.extend(ch.alerts.iter().filter(|a| !all.iter().any(|m| m.id == a.id)).cloned().collect::<Vec<_>>());
                all.sort_by_key(|m| m.ts);
            }
            all
        },
    }
}

/// Czy wiadomość oznacza zalogowanego streamera (słowo = login, bez własnych wiadomości).
fn mentions(text: &str, sender: &str, me: &str) -> bool {
    !sender.eq_ignore_ascii_case(me)
        && text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|w| w.eq_ignore_ascii_case(me))
}

fn demo_summary_text(s: &Summary) -> String {
    let top = s.top_chatters.first().map_or("czat", |(n, _)| n.as_str());
    format!(
        "Co to był za wieczór! Przez {} minut czat nie zwalniał ani na chwilę — {} wiadomości i mnóstwo KEKW. \
Najgłośniej było od {top}, który chyba nie odkleił się od klawiatury. Dzięki wszystkim za wsparcie, \
do zobaczenia na następnym streamie!",
        s.minutes, s.messages
    )
}

/// Login twórcy LiMBOCHAT na Twitchu.
const CREATOR: &str = "kosmiaczek";

const CREATOR_GREETINGS: [&str; 5] = [
    "Witam Szefa Kosmiego!",
    "Uwaga, uwaga! Na czacie pojawił się Szef Kosmi. Witamy serdecznie!",
    "Szef Kosmi na pokładzie! Kłaniam się nisko.",
    "Ooo, sam Szef Kosmi! Witam w imieniu całego czatu!",
    "Dzień dobry, Szefie Kosmi! Wszystko działa, jak należy.",
];

/// Czy wiadomość to przywitanie (po polsku, angielsku albo emotką).
fn is_greeting(text: &str) -> bool {
    let plain: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            c => c,
        })
        .collect();
    const PHRASES: [&str; 6] = ["dzien dobry", "dobry wieczor", "dobry dzien", "good morning", "good evening", "kope lat"];
    if PHRASES.iter().any(|p| plain.contains(p)) {
        return true;
    }
    const WORDS: [&str; 34] = [
        "siema", "siemka", "siemano", "siemanko", "siemaneczko", "siemson", "hej", "hejka", "hejo", "hejeczka", "heja", "elo",
        "eluwina", "elko", "witam", "witajcie", "witaj", "witka", "czesc", "czolem", "cze", "serwus", "jol", "joł", "hello", "hi",
        "hey", "yo", "hola", "heyy", "heyo", "heyguys", "vohiyo", "o/",
    ];
    plain
        .split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '\\'))
        .filter(|w| !w.is_empty())
        .take(6) // powitanie jest na początku wiadomości
        .any(|w| WORDS.contains(&w) || w == "\\o" || w == "o/" || w.starts_with("siemk") || w.starts_with("hejk"))
}

/// Opis zdarzenia dla rymowanki (None = bez rymowanki).
fn rhyme_event(alert: &ChatAlert) -> Option<String> {
    Some(match &alert.kind {
        AlertKind::Bits { amount } => format!("wysłał(a) {amount} bitsów"),
        AlertKind::Sub { months, resub: true, .. } => format!("subskrybuje kanał już {months} miesięcy"),
        AlertKind::Sub { tier, .. } if tier == "Prime" => "zasubskrybował(a) kanał przez Prime".into(),
        AlertKind::Sub { .. } => "właśnie zasubskrybował(a) kanał".into(),
        AlertKind::GiftSub { recipient, anonymous: false, .. } => format!("podarował(a) suba dla {recipient}"),
        AlertKind::MassGift { count, anonymous: false, .. } => format!("rozdał(a) {count} subów dla widzów"),
        AlertKind::Raid { viewers, .. } => format!("wpadł(a) z raidem i {viewers} widzami"),
        AlertKind::BitsBadge { threshold } => format!("zdobył(a) odznakę za {threshold} bitsów"),
        AlertKind::Follow => "zaczął/zaczęła obserwować kanał".into(),
        AlertKind::WatchStreak { streams } => format!("ogląda kanał już {streams} streamów z rzędu"),
        AlertKind::Redemption { reward, .. } => format!("odebrał(a) nagrodę za punkty kanału: {reward}"),
        _ => return None,
    })
}

fn alert_dto(alert: &ChatAlert) -> AlertDto {
    let mut d = AlertDto::default();
    match &alert.kind {
        AlertKind::Bits { amount } => {
            d.kind = "bits".into();
            d.amount = *amount;
        }
        AlertKind::Sub { tier, months, resub } => {
            d.kind = if *resub { "resub" } else { "sub" }.into();
            d.amount = *months;
            d.tier = Some(tier.clone());
        }
        AlertKind::GiftSub { tier, recipient, anonymous } => {
            d.kind = "gift".into();
            d.amount = 1;
            d.tier = Some(tier.clone());
            d.target = Some(recipient.clone());
            d.anonymous = *anonymous;
        }
        AlertKind::MassGift { tier, count, total, anonymous } => {
            d.kind = "mass_gift".into();
            d.amount = *count;
            d.tier = Some(tier.clone());
            d.anonymous = *anonymous;
            d.target = total.map(|t| t.to_string());
        }
        AlertKind::Raid { viewers, avatar } => {
            d.kind = "raid".into();
            d.amount = *viewers;
            d.avatar = Some(avatar.clone()).filter(|a| a.starts_with("https://"));
        }
        AlertKind::BitsBadge { threshold } => {
            d.kind = "bits_badge".into();
            d.amount = *threshold;
        }
        AlertKind::GiftUpgrade { gifter } => {
            d.kind = "upgrade".into();
            d.target = gifter.clone();
        }
        AlertKind::NewChatter => d.kind = "new_chatter".into(),
        AlertKind::WatchStreak { streams } => {
            d.kind = "watch_streak".into();
            d.amount = *streams;
        }
        AlertKind::Follow => d.kind = "follow".into(),
        AlertKind::Vip { title, greeting } => {
            d.kind = "vip".into();
            d.target = Some(title.clone());
            d.extra = Some(greeting.clone());
        }
        AlertKind::Redemption { reward, cost, input } => {
            d.kind = "redemption".into();
            d.amount = *cost;
            d.target = Some(reward.clone());
            d.extra = input.clone();
        }
    }
    d
}

#[cfg(test)]
mod mention_tests {
    #[test]
    fn greetings_are_detected() {
        for t in ["siema", "Siemka wszystkim", "hej hej", "Dzień dobry!", "elo elo 320", "Cześć Kosmi", "HeyGuys", "o/", "witam serdecznie", "yo"] {
            assert!(super::is_greeting(t), "{t}");
        }
        for t in ["ale gra", "GG", "kto gra w hejtera", "dobry stream", "hipopotam", "no i wtedy ktos mu odpisal ze jutro siema"] {
            assert!(!super::is_greeting(t), "{t}");
        }
    }

    #[test]
    fn detects_mentions() {
        assert!(super::mentions("@Kosmi siema", "widz", "kosmi"));
        assert!(super::mentions("kosmi, gramy?", "widz", "kosmi"));
        assert!(!super::mentions("kosmiczny stream", "widz", "kosmi"));
        assert!(!super::mentions("@kosmi test", "Kosmi", "kosmi"));
    }
}
