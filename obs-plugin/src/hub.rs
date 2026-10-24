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
};

/// Ile ostatnich wiadomości na kanał trzymamy dla przeładowanych paneli.
const HISTORY: usize = 300;

pub type Outbox = mpsc::UnboundedSender<Arc<str>>;

pub enum Request {
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

    pub fn update_status(&self, status: UpdateStatus) {
        let _ = self.requests.send(Request::UpdateStatus(status));
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
    helix: Option<Arc<Helix>>,
    ui_tx: mpsc::UnboundedSender<UiEvent>,
    me: HubHandle,
    eventsub: Option<tokio::task::JoinHandle<()>>,
    /// Gift-bomby w toku: (kanał, darczyńca, ile jeszcze pojedynczych gifów zwinąć, od kiedy, id alertu).
    gift_waves: Vec<(String, String, u64, Instant, String)>,
    demo_alert: usize,
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
        let jarvis = jarvis::spawn(
            rt,
            helix.clone(),
            events.clone(),
            HubHandle { requests: requests.clone() },
            config.jarvis.clone(),
            jarvis_status.clone(),
            demo_mode,
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
            helix,
            ui_tx: ui_tx.clone(),
            me: HubHandle { requests: requests.clone() },
            eventsub: None,
            gift_waves: Vec::new(),
            demo_alert: 0,
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
            loop {
                tokio::select! {
                    Some(event) = ui_rx.recv() => hub.on_event(event),
                    Some(request) = request_rx.recv() => hub.on_request(request),
                    _ = demo_tick.tick(), if hub.demo.is_some() => hub.demo_message(),
                    _ = shutdown.changed() => break,
                }
            }
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
            chatters,
            recent,
            demo: self.demo.is_some(),
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
            UiEvent::Alert(alert) => self.on_alert(*alert, false),
            UiEvent::Error { channel, text } => self.notice(channel, text, true),
        }
    }

    fn push_message(&mut self, msg: ParsedMessage) {
        let dto = MessageDto::from(&msg);
        if let (AuthState::LoggedIn { login, .. }, Some(j)) = (&self.auth, &self.jarvis) {
            if self.config.jarvis.read_mentions && mentions(&msg.raw_text, &msg.sender_login, login) {
                j.mention(msg.sender_name.clone(), msg.raw_text.clone());
            }
        }
        let Some(ch) = self.channel_mut(&msg.channel_login) else { return };
        ch.history.push_back(msg);
        if ch.history.len() > HISTORY {
            ch.history.pop_front();
        }
        while ch.history.len() + ch.carried.len() > HISTORY && ch.carried.pop_front().is_some() {}
        self.broadcast(&ServerMsg::Message { m: dto });
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
                let auto = self.config.updates != "ask";
                let idle = self.jarvis.as_ref().is_none_or(JarvisHandle::is_idle);
                let _ = reply.send((auto, idle));
            }
            Request::UpdateStatus(status) => self.broadcast(&ServerMsg::Update(status)),
            Request::AlertRhyme { channel, id, rhyme } => self.on_rhyme(&channel, &id, rhyme),
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
        AlertKind::Follow => d.kind = "follow".into(),
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
    fn detects_mentions() {
        assert!(super::mentions("@Kosmi siema", "widz", "kosmi"));
        assert!(super::mentions("kosmi, gramy?", "widz", "kosmi"));
        assert!(!super::mentions("kosmiczny stream", "widz", "kosmi"));
        assert!(!super::mentions("@kosmi test", "Kosmi", "kosmi"));
    }
}
