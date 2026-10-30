//! Jarvis — głosowy asystent streamera.
//!
//! Przebieg jednej komendy:
//! 1. skrót (domyślnie Shift+Z+X) lub przycisk w panelu → dźwięk „słucham”,
//! 2. nagrywanie z mikrofonu do chwili ciszy (równolegle pobieramy kontekst:
//!    kanał, tytuł, kategorię, osoby z czatu),
//! 3. rozpoznanie mowy: Groq Whisper (lub zapasowo Gemini),
//! 4. Gemini z narzędziami decyduje, co zrobić; wykonujemy akcje na Twitchu,
//! 5. odpowiedź: napis w panelu + głos: naturalny Gemini (moduł `tts`),
//!    a gdy niedostępny — polski głos systemowy.
//!
//! Ponowne naciśnięcie skrótu w trakcie słuchania kończy nagrywanie od razu;
//! w trakcie mówienia — ucisza Jarvisa i zaczyna słuchać nowej komendy.
//! Jarvis pamięta rozmowę przez 90 s (dopytania, potwierdzenia).
//!
//! Zamiast skrótu można powiedzieć „Hej Limbo” (moduł `wake`), także od razu
//! z komendą: „Hej Limbo, zmień kategorię na GTA 4”.

pub mod audio;
pub mod dsp;
pub mod gemini;
pub mod hotkey;
pub mod openai;
pub mod secrets;
pub mod stt;
pub mod tools;
pub mod tts;
pub mod twitch;
pub mod usage;
pub mod voice;
pub mod wake;

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    runtime::Handle,
    sync::{broadcast, mpsc},
};

use crate::{
    helix::Helix,
    hub::HubHandle,
    protocol::ServerMsg,
};
use secrets::Provider;
use voice::{Chime, Voice};

const MEMORY: Duration = Duration::from_secs(90);
const MAX_ROUNDS: usize = 5;
const MAX_HISTORY: usize = 30;
/// Najdłużej czekamy na głos Gemini, potem mówi głos systemowy.
const TTS_TIMEOUT: Duration = Duration::from_secs(8);
/// Po błędzie głosu Gemini (limit, brak sieci) przez ten czas mówi od razu głos systemowy.
const TTS_PAUSE: Duration = Duration::from_secs(5 * 60);

pub const MODELS: &[(&str, &str)] = &[
    ("gemini-3.5-flash-lite", "Gemini 3.5 Flash-Lite — najszybszy"),
    ("gemini-3.8-flash", "Gemini 3.8 Flash — mądrzejszy, wolniejszy"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JarvisConfig {
    pub enabled: bool,
    pub voice: bool,
    pub voice_volume:u8,
    /// Głos: `gemini` (naturalny, w chmurze; zapasowo systemowy) albo `system` (głos Windows).
    pub voice_engine: String,
    /// Głos Gemini — nazwa z `tts::VOICES`.
    pub tts_voice: String,
    pub hotkey: Vec<String>,
    pub mic: Option<String>,
    pub model: String,
    /// Aktywacja głosem „Hej Limbo” (wymaga nauczenia głosu).
    pub wake_word: bool,
    /// Czułość „Hej Limbo”: 1 = niska, 2 = średnia, 3 = wysoka.
    pub wake_sensitivity: u8,
    /// Czytaj na głos wiadomości, w których ktoś Cię oznaczy.
    pub read_mentions: bool,
    /// Mózg: `gemini` albo `custom` (własny model zgodny z OpenAI).
    pub brain: String,
    /// Preset własnego modelu: openai / openrouter / groq / ollama / lmstudio / custom.
    pub llm_preset: String,
    /// Adres API (tylko dla presetu `custom`).
    pub llm_url: String,
    pub llm_model: String,
    /// `toggle` — naciśnij, mów, naciśnij/zamilknij; `hold` — trzymaj klawisz podczas mówienia.
    pub talk_mode: String,
}

impl Default for JarvisConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            voice: true,
            voice_volume:100,
            voice_engine: "gemini".into(),
            tts_voice: tts::DEFAULT_VOICE.into(),
            hotkey: vec!["Shift".into(), "Z".into(), "X".into()],
            mic: None,
            model: MODELS[0].0.into(),
            wake_word: false,
            wake_sensitivity: 2,
            read_mentions: false,
            brain: "gemini".into(),
            llm_preset: "openai".into(),
            llm_url: String::new(),
            llm_model: String::new(),
            talk_mode: "toggle".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct LlmPreset {
    pub id: String,
    pub name: String,
    pub url: String,
    pub model: String,
    pub needs_key: bool,
}

/// Stan pokazywany w ustawieniach panelu (bez kluczy — tylko czy są).
#[derive(Debug, Clone, Serialize, Default)]
pub struct JarvisStatus {
    pub config: JarvisConfig,
    pub gemini_key: bool,
    pub groq_key: bool,
    pub llm_key: bool,
    pub llm_presets: Vec<LlmPreset>,
    pub mics: Vec<String>,
    pub polish_voice: Option<bool>,
    pub hotkey_supported: bool,
    pub models: Vec<(String, String)>,
    /// Głosy Gemini do wyboru (nazwa, opis).
    pub tts_voices: Vec<(String, String)>,
    /// Zapytania do AI dzisiaj (dzień Google).
    pub usage: usage::Usage,
    pub busy: bool,
    /// Czy „Hej Limbo” jest nauczone (3 nagrania zapisane).
    pub wake_trained: bool,
    /// Czy nasłuch „Hej Limbo” właśnie działa.
    pub wake_listening: bool,
    pub wake_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "e", rename_all = "snake_case")]
pub enum JarvisEvent {
    State { state: &'static str },
    Level { v: f32 },
    Transcript { text: String },
    Action { text: String, ok: bool, url: Option<String> },
    Reply { text: String },
    Error { text: String },
    Status(JarvisStatus),
    /// Przypomnienie ustawione wcześniej głosem.
    Reminder { text: String },
    /// Powitanie VIP-a (np. twórcy) — złota karta z koroną na overlayu.
    Vip { name: String, title: String, text: String, creator: bool },
    /// Nauka „Hej Limbo”: krok 1–3, stan: listening / ok / retry / done / cancelled / error.
    Enroll { step: usize, total: usize, state: &'static str, text: String },
    /// Lista modeli z `/models` własnego dostawcy.
    Models { models: Vec<String>, error: Option<String> },
}

enum Cmd {
    Activate,
    HotkeyDown,
    HotkeyUp,
    ListModels,
    SetKey(Provider, Option<String>),
    SetConfig(JarvisConfig),
    Refresh,
    WakeCandidate(Vec<u8>),
    WakeError(String),
    WakeRestart,
    Enroll(String),
    Mention { name: String, text: String },
    Rhyme { nick: String, event: String, reply: tokio::sync::oneshot::Sender<Option<String>> },
    Ask { system: &'static str, prompt: String, reply: tokio::sync::oneshot::Sender<Option<String>> },
    Say(String),
}

#[derive(Clone)]
pub struct JarvisHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    idle: Arc<AtomicBool>,
}

impl JarvisHandle {
    /// Nic się nie dzieje (nie słucha, nie myśli, nie mówi, nie uczy się) — można np. zaktualizować silnik.
    pub fn is_idle(&self) -> bool {
        self.idle.load(Ordering::Relaxed)
    }

    pub fn activate(&self) {
        let _ = self.tx.send(Cmd::Activate);
    }

    pub fn set_key(&self, provider: Provider, key: Option<String>) {
        let _ = self.tx.send(Cmd::SetKey(provider, key));
    }

    pub fn set_config(&self, config: JarvisConfig) {
        let _ = self.tx.send(Cmd::SetConfig(config));
    }

    pub fn refresh(&self) {
        let _ = self.tx.send(Cmd::Refresh);
    }

    /// Pobiera listę modeli od wybranego dostawcy (odpowiedź: `JarvisEvent::Models`).
    pub fn list_models(&self) {
        let _ = self.tx.send(Cmd::ListModels);
    }

    /// `start` / `cancel` / `reset` nauki „Hej Limbo”.
    pub fn enroll(&self, action: &str) {
        let _ = self.tx.send(Cmd::Enroll(action.to_owned()));
    }

    /// Rymowanka do nicku osoby z alertu (None = brak mózgu / błąd / za dużo naraz).
    pub fn rhyme(&self, nick: String, event: String) -> tokio::sync::oneshot::Receiver<Option<String>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(Cmd::Rhyme { nick, event, reply });
        rx
    }

    /// Jedno pytanie do mózgu LimboAI bez narzędzi (podsumowanie streama, fakty o widzach).
    pub fn ask(&self, system: &'static str, prompt: String) -> tokio::sync::oneshot::Receiver<Option<String>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(Cmd::Ask { system, prompt, reply });
        rx
    }

    /// Przeczytaj na głos (gdy głos włączony i LimboAI nic nie robi).
    pub fn say(&self, text: String) {
        let _ = self.tx.send(Cmd::Say(text));
    }

    /// Ktoś oznaczył streamera — Jarvis może to przeczytać na głos.
    pub fn mention(&self, name: String, text: String) {
        let _ = self.tx.send(Cmd::Mention { name, text });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Listening,
    Thinking,
    Speaking,
}

struct Brain {
    executor: tools::Executor,
    /// Format historii zależy od mózgu — po zmianie mózgu zaczynamy od nowa.
    kind: &'static str,
    history: Vec<Value>,
    last: Option<Instant>,
}

struct Shared {
    http: reqwest::Client,
    helix: Option<Arc<Helix>>,
    events: broadcast::Sender<Arc<str>>,
    hub: HubHandle,
    voice: Voice,
    status: Arc<Mutex<JarvisStatus>>,
    brain: tokio::sync::Mutex<Brain>,
    phase: Mutex<Phase>,
    /// Zakończ nagrywanie (ponowny skrót).
    stop: Mutex<Arc<AtomicBool>>,
    /// Numer sesji — chroni przed nadpisaniem stanu przez starą sesję.
    session: AtomicU64,
    demo: bool,
    /// Nasłuch „Hej Limbo” wstrzymany do tej chwili (po mówieniu Jarvisa).
    wake_hold: Mutex<Instant>,
    /// Trwa sprawdzanie kandydata „Hej Limbo” w chmurze.
    wake_confirming: AtomicBool,
    /// Ostatnie zapytania potwierdzające (limit na minutę).
    wake_requests: Mutex<VecDeque<Instant>>,
    enrolling: AtomicBool,
    enroll_cancel: Mutex<Arc<AtomicBool>>,
    reminder_generation: AtomicU64,
    last_mention: Mutex<Option<Instant>>,
    idle: Arc<AtomicBool>,
    /// Rymowanki: jedna naraz, z odstępem; kolejka najwyżej kilku.
    rhyme_gate: tokio::sync::Mutex<Option<Instant>>,
    rhyme_waiting: std::sync::atomic::AtomicUsize,
    /// Do tej chwili głos Gemini wstrzymany po błędzie (mówi głos systemowy).
    tts_pause: Mutex<Option<Instant>>,
}

impl Shared {
    fn emit(&self, ev: JarvisEvent) {
        if let Ok(json) = serde_json::to_string(&ServerMsg::Jarvis { ev }) {
            let _ = self.events.send(json.into());
        }
    }

    fn set_phase(&self, phase: Phase) {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner()) = phase;
        self.idle.store(phase == Phase::Idle && !self.enrolling.load(Ordering::Relaxed), Ordering::Relaxed);
        // Po każdej zmianie chwila przerwy w nasłuchu „Hej Limbo” (echo głosu Jarvisa).
        self.hold_wake(Duration::from_millis(900));
        let state = match phase {
            Phase::Idle => "idle",
            Phase::Listening => "listening",
            Phase::Thinking => "thinking",
            Phase::Speaking => "speaking",
        };
        self.emit(JarvisEvent::State { state });
    }

    fn phase(&self) -> Phase {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn hold_wake(&self, duration: Duration) {
        let mut until = self.wake_hold.lock().unwrap_or_else(|e| e.into_inner());
        *until = (*until).max(Instant::now() + duration);
    }

    /// Czy nasłuch „Hej Limbo” ma teraz ignorować dźwięk.
    fn wake_paused(&self) -> bool {
        self.phase() != Phase::Idle
            || self.enrolling.load(Ordering::Relaxed)
            || self.wake_confirming.load(Ordering::Relaxed)
            || Instant::now() < *self.wake_hold.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn update_status(&self, f: impl FnOnce(&mut JarvisStatus)) {
        let status = {
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut s);
            s.clone()
        };
        self.emit(JarvisEvent::Status(status));
    }

    fn config(&self) -> JarvisConfig {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).config.clone()
    }

    fn notice(&self, channel: Option<String>, text: String, error: bool) {
        if let Ok(json) = serde_json::to_string(&ServerMsg::Notice { channel, text, error }) {
            let _ = self.events.send(json.into());
        }
    }
}

pub fn spawn(
    rt: &Handle,
    helix: Option<Arc<Helix>>,
    events: broadcast::Sender<Arc<str>>,
    hub: HubHandle,
    config: JarvisConfig,
    status: Arc<Mutex<JarvisStatus>>,
    demo: bool,
    obs: Option<crate::obs::ObsHandle>,
    memory: Option<Arc<crate::memory::Memory>>,
) -> JarvisHandle {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (reminder_tx, mut reminder_rx) = mpsc::unbounded_channel();
    {
        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
        s.config = config.clone();
        s.hotkey_supported = hotkey::SUPPORTED;
        s.models = MODELS.iter().map(|(id, label)| ((*id).into(), (*label).into())).collect();
        s.tts_voices = tts::VOICES.iter().map(|(id, label)| ((*id).into(), (*label).into())).collect();
        s.llm_presets = openai::PRESETS
            .iter()
            .map(|(id, name, url, model, needs_key)| LlmPreset {
                id: (*id).into(),
                name: (*name).into(),
                url: (*url).into(),
                model: (*model).into(),
                needs_key: *needs_key,
            })
            .collect();
    }

    let shared = Arc::new(Shared {
        http: crate::backend::build_http_client(),
        helix: helix.clone(),
        events,
        hub: hub.clone(),
        voice: Voice::new(config.voice,config.voice_volume),
        status,
        brain: tokio::sync::Mutex::new(Brain {
            executor: tools::Executor::new(helix, tools::Local { hub: Some(hub.clone()), reminders: Some(reminder_tx), obs, memory }),
            kind: "gemini",
            history: Vec::new(),
            last: None,
        }),
        phase: Mutex::new(Phase::Idle),
        stop: Mutex::new(Arc::new(AtomicBool::new(false))),
        session: AtomicU64::new(0),
        demo,
        wake_hold: Mutex::new(Instant::now()),
        wake_confirming: AtomicBool::new(false),
        wake_requests: Mutex::new(VecDeque::new()),
        enrolling: AtomicBool::new(false),
        enroll_cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
        reminder_generation: AtomicU64::new(0),
        last_mention: Mutex::new(None),
        idle: Arc::new(AtomicBool::new(true)),
        rhyme_gate: tokio::sync::Mutex::new(None),
        rhyme_waiting: std::sync::atomic::AtomicUsize::new(0),
        tts_pause: Mutex::new(None),
    });

    let hotkey_tx = tx.clone();
    let combo = active_combo(&config);
    let hotkey = hotkey::Hotkey::spawn(combo, move |down| {
        let _ = hotkey_tx.send(if down { Cmd::HotkeyDown } else { Cmd::HotkeyUp });
    });

    let idle = shared.idle.clone();
    let wake_tx = tx.clone();
    rt.spawn(async move {
        let _hotkey = &hotkey;
        let mut listener: Option<wake::Listener> = None;
        refresh_status(&shared).await;
        restart_wake(&shared, &mut listener, &wake_tx);
        // Uruchom syntezator od razu — pierwsza odpowiedź nie będzie czekać na start.
        if config.voice {
            shared.voice.stop();
        }
        loop {
            let cmd = tokio::select! {
                Some(cmd) = rx.recv() => cmd,
                Some(reminder) = reminder_rx.recv() => {
                    handle_reminder(&shared, reminder);
                    continue;
                }
                else => break,
            };
            match cmd {
                Cmd::Activate => activate(&shared, false),
                Cmd::HotkeyDown => activate(&shared, shared.config().talk_mode == "hold"),
                Cmd::HotkeyUp => {
                    // Puszczenie klawisza w trybie „przytrzymaj” = koniec mówienia.
                    if shared.config().talk_mode == "hold" && shared.phase() == Phase::Listening {
                        shared.stop.lock().unwrap_or_else(|e| e.into_inner()).store(true, Ordering::Relaxed);
                    }
                }
                Cmd::ListModels => {
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        let config = shared.config();
                        let base = openai::base_url(&config.llm_preset, &config.llm_url);
                        let result = match openai::validate_url(&base) {
                            Err(e) => Err(e),
                            Ok(()) => {
                                let key = llm_key(&config).await;
                                openai::list_models(&shared.http, &base, key.as_deref()).await
                            }
                        };
                        let (models, error) = match result {
                            Ok(m) if m.is_empty() => (m, Some("Dostawca nie zwrócił listy modeli — wpisz nazwę ręcznie.".to_owned())),
                            Ok(m) => (m, None),
                            Err(e) => (Vec::new(), Some(e)),
                        };
                        shared.emit(JarvisEvent::Models { models, error });
                    });
                }
                Cmd::SetKey(provider, key) => {
                    match secrets::store(provider, key.clone()).await {
                        Ok(()) => {
                            let text = if key.is_some() { "zapisany" } else { "usunięty" };
                            shared.notice(None, format!("LimboAI: klucz {} {text}.", provider.name()), false);
                        }
                        Err(e) => shared.emit(JarvisEvent::Error { text: format!("Nie udało się zapisać klucza: {e:#}") }),
                    }
                    refresh_status(&shared).await;
                }
                Cmd::SetConfig(config) => {
                    hotkey.set(active_combo(&config));
                    shared.voice.set_enabled(config.voice);
                    shared.voice.set_volume(config.voice_volume);
                    let previous = std::mem::replace(&mut shared.status.lock().unwrap_or_else(|e| e.into_inner()).config, config.clone());
                    let wake_changed = previous.wake_word != config.wake_word
                        || previous.mic != config.mic
                        || previous.wake_sensitivity != config.wake_sensitivity
                        || previous.enabled != config.enabled;
                    if wake_changed {
                        restart_wake(&shared, &mut listener, &wake_tx);
                    }
                    refresh_status(&shared).await;
                }
                Cmd::Refresh => refresh_status(&shared).await,
                Cmd::WakeRestart => {
                    restart_wake(&shared, &mut listener, &wake_tx);
                    refresh_status(&shared).await;
                }
                Cmd::WakeCandidate(wav) => on_wake_candidate(&shared, wav),
                Cmd::WakeError(message) => {
                    listener = None;
                    tracing::warn!("nasłuch „Hej Limbo” zatrzymany: {message}");
                    shared.update_status(|s| {
                        s.wake_listening = false;
                        s.wake_error = Some(message.clone());
                    });
                    shared.notice(None, format!("LimboAI: nasłuch „Hej Limbo” wyłączony — {message}"), true);
                }
                Cmd::Enroll(action) => match action.as_str() {
                    "start" => start_enrollment(&shared, wake_tx.clone()),
                    "cancel" => shared.enroll_cancel.lock().unwrap_or_else(|e| e.into_inner()).store(true, Ordering::Relaxed),
                    "reset" => {
                        wake::delete_model();
                        restart_wake(&shared, &mut listener, &wake_tx);
                        refresh_status(&shared).await;
                    }
                    _ => {}
                },
                Cmd::Mention { name, text } => read_mention(&shared, &name, &text),
                Cmd::Rhyme { nick, event, reply } => {
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        let _ = reply.send(rhyme(&shared, &nick, &event).await);
                    });
                }
                Cmd::Ask { system, prompt, reply } => {
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        let answer = ask_brain(&shared, system, &prompt, 1200).await;
                        let _ = reply.send(answer);
                    });
                }
                Cmd::Say(text) => {
                    let config = shared.config();
                    if config.enabled && config.voice && shared.phase() == Phase::Idle && !shared.enrolling.load(Ordering::Relaxed) {
                        let text = clean_reply(&text);
                        shared.hold_wake(Duration::from_secs_f32(Voice::estimate_seconds(&text) + 1.0));
                        speak_later(&shared, text);
                    }
                }
            }
        }
    });

    JarvisHandle { tx, idle }
}

fn active_combo(config: &JarvisConfig) -> Vec<u16> {
    if !config.enabled {
        return Vec::new();
    }
    hotkey::parse_combo(&config.hotkey).unwrap_or_else(|| vec![0x10, 0x5A, 0x58])
}

async fn refresh_status(shared: &Arc<Shared>) {
    let gemini = secrets::load(Provider::Gemini).await.is_some();
    let groq = secrets::load(Provider::Groq).await.is_some();
    let llm = secrets::load(Provider::Llm).await.is_some();
    let mics = tokio::task::spawn_blocking(audio::input_devices).await.unwrap_or_default();
    let status = {
        let mut s = shared.status.lock().unwrap_or_else(|e| e.into_inner());
        s.gemini_key = gemini;
        s.groq_key = groq;
        s.llm_key = llm;
        s.mics = mics;
        s.polish_voice = shared.voice.polish_voice();
        s.busy = shared.phase() != Phase::Idle;
        s.wake_trained = wake::load_model().is_some();
        s.usage = usage::today();
        s.clone()
    };
    shared.emit(JarvisEvent::Status(status));
}

/// `hold`: nagrywanie trwa do puszczenia klawisza (tryb „przytrzymaj, żeby mówić”).
fn activate(shared: &Arc<Shared>, hold: bool) {
    let config = shared.config();
    if !config.enabled {
        return;
    }
    match shared.phase() {
        // Drugie naciśnięcie w trakcie słuchania = „skończyłem mówić”.
        Phase::Listening if !hold => shared.stop.lock().unwrap_or_else(|e| e.into_inner()).store(true, Ordering::Relaxed),
        Phase::Listening => {}
        Phase::Thinking => {}
        Phase::Idle | Phase::Speaking => {
            if shared.enrolling.load(Ordering::Relaxed) {
                return;
            }
            start_session(shared, None, hold);
        }
    }
}

/// Nowa komenda: z nagrywaniem (`None`) albo z gotowym tekstem z „Hej Limbo, …”.
fn start_session(shared: &Arc<Shared>, preset: Option<String>, hold: bool) {
    let config = shared.config();
    shared.voice.stop();
    let stop = Arc::new(AtomicBool::new(false));
    *shared.stop.lock().unwrap_or_else(|e| e.into_inner()) = stop.clone();
    let id = shared.session.fetch_add(1, Ordering::SeqCst) + 1;
    shared.set_phase(if preset.is_some() { Phase::Thinking } else { Phase::Listening });
    let shared = shared.clone();
    tokio::spawn(async move {
        if let Err(message) = session(&shared, id, stop, &config, preset, hold).await {
            shared.voice.chime(Chime::Error);
            shared.emit(JarvisEvent::Error { text: message.clone() });
            if shared.session.load(Ordering::SeqCst) == id {
                shared.set_phase(Phase::Idle);
            }
        }
    });
}

/// Jedna komenda głosowa. Błąd = komunikat dla użytkownika.
async fn session(
    shared: &Arc<Shared>,
    id: u64,
    stop: Arc<AtomicBool>,
    config: &JarvisConfig,
    preset: Option<String>,
    hold: bool,
) -> Result<(), String> {
    shared.voice.chime(Chime::Listen);
    let gemini_key = secrets::load(Provider::Gemini).await;
    let groq_key = secrets::load(Provider::Groq).await;
    let mind = match resolve_mind(config, gemini_key.clone()).await {
        Ok(mind) => Some(mind),
        // Tryb demo bez kluczy: pokazowy przebieg.
        Err(_) if shared.demo => None,
        Err(message) => return Err(message),
    };
    let scripted_demo = mind.is_none();
    if !scripted_demo && groq_key.is_none() && gemini_key.is_none() {
        return Err("Do rozpoznawania mowy potrzebny jest klucz Groq (darmowy) albo Gemini — dodaj go w ustawieniach (⚙ → LimboAI).".into());
    }

    if let Some(transcript) = preset {
        // „Hej Limbo, zmień kategorię…” — tekst już mamy, od razu do mózgu.
        let ctx = shared.hub.jarvis_context().await.unwrap_or_default();
        let stream = stream_summary(shared, &ctx).await;
        shared.emit(JarvisEvent::Transcript { text: transcript.clone() });
        return respond(shared, id, &ctx, &stream, &transcript, mind.as_ref()).await;
    }

    // Nagrywanie i pobieranie kontekstu równolegle.
    let level_shared = shared.clone();
    let mic = config.mic.clone();
    let controls = audio::Controls { stop, cancel: Arc::new(AtomicBool::new(false)), hold };
    let recording = tokio::task::spawn_blocking(move || {
        audio::record(mic.as_deref(), &controls, &|v| level_shared.emit(JarvisEvent::Level { v }))
    });
    let context = async {
        let ctx = shared.hub.jarvis_context().await.unwrap_or_default();
        let stream = stream_summary(shared, &ctx).await;
        (ctx, stream)
    };
    let (recording, (ctx, stream)) = tokio::join!(recording, context);

    let outcome = recording.map_err(|e| format!("Błąd nagrywania: {e}"))?.map_err(|e| format!("{e:#}"))?;
    let wav = match outcome {
        audio::Outcome::Speech(rec) => rec.wav,
        audio::Outcome::NoSpeech => {
            shared.voice.chime(Chime::Stop);
            shared.emit(JarvisEvent::Error { text: "Nie usłyszałem komendy — spróbuj jeszcze raz.".into() });
            shared.set_phase(Phase::Idle);
            return Ok(());
        }
        audio::Outcome::Cancelled => {
            shared.set_phase(Phase::Idle);
            return Ok(());
        }
    };
    shared.voice.chime(Chime::Stop);
    shared.set_phase(Phase::Thinking);

    // Rozpoznanie mowy.
    let names: Vec<String> = ctx.chatters.iter().map(|c| c.name.clone()).collect();
    let prompt = stt::build_prompt(&names);
    let transcript = if scripted_demo {
        tokio::time::sleep(Duration::from_millis(500)).await;
        "Zmień kategorię na GTA 4".to_owned()
    } else if let Some(key) = &groq_key {
        stt::transcribe_groq(&shared.http, key, wav, &prompt).await.map_err(|e| format!("{e:#}"))?
    } else {
        let key = gemini_key.as_deref().unwrap_or_default();
        stt::transcribe_gemini(&shared.http, key, &config.model, &wav, &prompt).await.map_err(|e| format!("{e:#}"))?
    };
    if transcript.trim().is_empty() {
        return Err("Nie zrozumiałem — powtórz, proszę.".into());
    }
    shared.emit(JarvisEvent::Transcript { text: transcript.clone() });
    respond(shared, id, &ctx, &stream, &transcript, mind.as_ref()).await
}

/// Wybrany „mózg” z gotowymi danymi dostępu.
enum Mind {
    Gemini { key: String, model: String },
    OpenAi { base: String, key: Option<String>, model: String },
}

impl Mind {
    fn kind(&self) -> &'static str {
        match self {
            Self::Gemini { .. } => "gemini",
            Self::OpenAi { .. } => "openai",
        }
    }
}

/// Klucz własnego modelu; dla presetu Groq wystarczy klucz Groq z rozpoznawania mowy.
async fn llm_key(config: &JarvisConfig) -> Option<String> {
    match secrets::load(Provider::Llm).await {
        Some(key) => Some(key),
        None if config.llm_preset == "groq" => secrets::load(Provider::Groq).await,
        None => None,
    }
}

async fn resolve_mind(config: &JarvisConfig, gemini_key: Option<String>) -> Result<Mind, String> {
    if config.brain != "custom" {
        let key = gemini_key.ok_or("LimboAI potrzebuje klucza Gemini (albo wybierz własny model) — ⚙ → LimboAI.")?;
        return Ok(Mind::Gemini { key, model: config.model.clone() });
    }
    let base = openai::base_url(&config.llm_preset, &config.llm_url);
    openai::validate_url(&base)?;
    let model = config.llm_model.trim();
    if model.is_empty() {
        return Err("Wpisz nazwę modelu w ustawieniach (⚙ → LimboAI → Własny model).".into());
    }
    let key = llm_key(config).await;
    let needs_key = openai::preset(&config.llm_preset).is_some_and(|p| p.4);
    if key.is_none() && needs_key {
        return Err("Dodaj klucz API własnego modelu w ustawieniach (⚙ → LimboAI).".into());
    }
    Ok(Mind::OpenAi { base, key, model: model.to_owned() })
}

/// Mózg + narzędzia + odpowiedź (napis i głos). `mind: None` = pokaz demo.
async fn respond(
    shared: &Arc<Shared>,
    id: u64,
    ctx: &tools::Context,
    stream: &Option<(String, String)>,
    transcript: &str,
    mind: Option<&Mind>,
) -> Result<(), String> {
    let (reply, actions) = match mind {
        None => scripted_reply(shared, ctx).await,
        Some(mind) => think(shared, ctx, stream, transcript, mind).await?,
    };

    let reply = if reply.is_empty() {
        if actions.iter().any(|a| a.ok) { "Zrobione.".to_owned() } else { "Nie udało się — szczegóły w panelu.".to_owned() }
    } else {
        reply
    };
    shared.emit(JarvisEvent::Reply { text: reply.clone() });
    for url in actions.iter().filter(|a| a.ok).filter_map(|a| a.url.clone()) {
        shared.hub.clip_made(url);
    }
    for action in actions.iter().filter(|a| a.ok && !a.text.starts_with('❔')) {
        let channel = ctx.active.as_ref().map(|c| c.login.clone()).or_else(|| ctx.me.as_ref().map(|(l, _)| l.clone()));
        shared.notice(channel, format!("🤖 LimboAI: {}", action.text), false);
    }

    if shared.session.load(Ordering::SeqCst) != id {
        return Ok(());
    }
    if shared.config().voice {
        shared.set_phase(Phase::Speaking);
        let length = speak(shared, &reply).await;
        tokio::time::sleep(length).await;
    }
    if shared.session.load(Ordering::SeqCst) == id && shared.phase() != Phase::Listening {
        shared.set_phase(Phase::Idle);
    }
    Ok(())
}

// ── „Hej Limbo” ────────────────────────────────────────────────────────

/// (Re)startuje nasłuch zgodnie z ustawieniami i nauczonym modelem.
fn restart_wake(shared: &Arc<Shared>, listener: &mut Option<wake::Listener>, tx: &mpsc::UnboundedSender<Cmd>) {
    *listener = None;
    let config = shared.config();
    let model = wake::load_model();
    let wanted = config.enabled && config.wake_word;
    if let (true, Some(model)) = (wanted, model) {
        let (paused_shared, candidate_tx, error_tx) = (shared.clone(), tx.clone(), tx.clone());
        *listener = Some(wake::spawn(
            config.mic.clone(),
            &model,
            config.wake_sensitivity,
            move || paused_shared.wake_paused(),
            move |wav, distance| {
                tracing::debug!(distance, "kandydat „Hej Limbo”");
                let _ = candidate_tx.send(Cmd::WakeCandidate(wav));
            },
            move |message| {
                let _ = error_tx.send(Cmd::WakeError(message));
            },
        ));
    }
    let listening = listener.is_some();
    let mut s = shared.status.lock().unwrap_or_else(|e| e.into_inner());
    s.wake_listening = listening;
    s.wake_error = None;
}

/// Kandydat z lokalnego detektora → potwierdzenie transkrypcją → komenda.
fn on_wake_candidate(shared: &Arc<Shared>, wav: Vec<u8>) {
    let config = shared.config();
    if !config.enabled || !config.wake_word || shared.phase() != Phase::Idle || shared.enrolling.load(Ordering::Relaxed) {
        return;
    }
    // Limit zapytań: najwyżej 1 na 1,2 s i 15 na minutę (głośny stream nie zje limitu Groq).
    {
        let mut requests = shared.wake_requests.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        while requests.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
            requests.pop_front();
        }
        if requests.len() >= 15 || requests.back().is_some_and(|t| now.duration_since(*t) < Duration::from_millis(1200)) {
            return;
        }
        requests.push_back(now);
    }
    if shared.wake_confirming.swap(true, Ordering::SeqCst) {
        return;
    }
    let shared = shared.clone();
    tokio::spawn(async move {
        let transcript = confirm_wake(&shared, &config, wav).await;
        shared.wake_confirming.store(false, Ordering::SeqCst);
        let Some(text) = transcript else { return };
        match wake::parse_wake(&text) {
            Some(hit) if shared.phase() == Phase::Idle => {
                tracing::info!(text, "„Hej Limbo” rozpoznane");
                start_session(&shared, hit.command, false);
            }
            _ => tracing::debug!(text, "to nie było „Hej Limbo”"),
        }
    });
}

async fn confirm_wake(shared: &Arc<Shared>, config: &JarvisConfig, wav: Vec<u8>) -> Option<String> {
    let prompt = "Hej Limbo, zmień kategorię. Hej Limbo, daj timeout.";
    if let Some(key) = secrets::load(Provider::Groq).await {
        return stt::transcribe_groq(&shared.http, &key, wav, prompt).await.map_err(|e| tracing::warn!("Hej Limbo: {e:#}")).ok();
    }
    if let Some(key) = secrets::load(Provider::Gemini).await {
        return stt::transcribe_gemini(&shared.http, &key, &config.model, &wav, prompt)
            .await
            .map_err(|e| tracing::warn!("Hej Limbo: {e:#}"))
            .ok();
    }
    // Tryb demo bez kluczy: każde dopasowanie lokalne traktujemy jako wezwanie.
    shared.demo.then(|| "Hej Limbo, zmień kategorię na GTA 4".to_owned())
}

/// Nauka „Hej Limbo”: 3 nagrania → model zapisany lokalnie.
fn start_enrollment(shared: &Arc<Shared>, tx: mpsc::UnboundedSender<Cmd>) {
    if shared.phase() != Phase::Idle || shared.enrolling.swap(true, Ordering::SeqCst) {
        return;
    }
    shared.idle.store(false, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    *shared.enroll_cancel.lock().unwrap_or_else(|e| e.into_inner()) = cancel.clone();
    let shared = shared.clone();
    tokio::spawn(async move {
        let total = wake::ENROLL_SAMPLES;
        let emit = |step: usize, state: &'static str, text: &str| {
            shared.emit(JarvisEvent::Enroll { step, total, state, text: text.to_owned() });
        };
        let mut samples = Vec::new();
        let mut attempts = 0;
        while samples.len() < total {
            let step = samples.len() + 1;
            attempts += 1;
            if attempts > total * 3 {
                emit(step, "error", "Nie udało się nagrać — sprawdź mikrofon i spróbuj ponownie.");
                break;
            }
            emit(step, "listening", "Powiedz wyraźnie: „Hej Limbo”");
            shared.voice.chime(Chime::Listen);
            let mic = shared.config().mic;
            let controls = audio::Controls { stop: Arc::new(AtomicBool::new(false)), cancel: cancel.clone(), hold: false };
            let level_shared = shared.clone();
            let recorded = tokio::task::spawn_blocking(move || {
                audio::record(mic.as_deref(), &controls, &|v| level_shared.emit(JarvisEvent::Level { v }))
            })
            .await;
            match recorded {
                Ok(Ok(audio::Outcome::Speech(rec))) => match wake::enrollment_features(&rec.wav) {
                    Ok(features) => {
                        samples.push(features);
                        shared.voice.chime(Chime::Stop);
                        emit(step, "ok", "Mam!");
                    }
                    Err(message) => {
                        shared.voice.chime(Chime::Error);
                        emit(step, "retry", &message);
                    }
                },
                Ok(Ok(audio::Outcome::NoSpeech)) => {
                    shared.voice.chime(Chime::Error);
                    emit(step, "retry", "Nic nie usłyszałem — mów trochę głośniej.");
                }
                Ok(Ok(audio::Outcome::Cancelled)) => {
                    emit(step, "cancelled", "Anulowano.");
                    break;
                }
                Ok(Err(e)) => {
                    emit(step, "error", &format!("{e:#}"));
                    break;
                }
                Err(e) => {
                    emit(step, "error", &format!("Błąd nagrywania: {e}"));
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
        if samples.len() == total {
            match wake::build_model(&samples).and_then(|m| wake::save_model(&m).map_err(|e| format!("Nie udało się zapisać: {e:#}"))) {
                Ok(()) => emit(total, "done", "Gotowe! Powiedz „Hej Limbo”, a LimboAI się zgłosi."),
                Err(message) => emit(total, "error", &message),
            }
        }
        shared.enrolling.store(false, Ordering::SeqCst);
        shared.idle.store(shared.phase() == Phase::Idle, Ordering::Relaxed);
        let _ = tx.send(Cmd::WakeRestart);
    });
}

// ── Rymowanki do alertów ───────────────────────────────────────────────

const RHYME_SYSTEM: &str = "Jesteś LimboAI, asystent streamera na Twitchu. Układasz krótkie, zabawne i życzliwe \
rymowanki po polsku dla widzów, którzy właśnie wsparli stream.
Zasady:
- dokładnie dwa wersy, które się rymują, razem najwyżej 18 słów,
- użyj nicku widza (możesz go odmienić albo zdrobnić, ale nie zmieniaj liter),
- ciepło i z humorem; bez obrażania, wulgaryzmów, polityki i aluzji erotycznych,
- bez emoji, hashtagów, cudzysłowów i wstępu — tylko same dwa wersy w osobnych liniach.";

/// Jedno pytanie do mózgu (Gemini albo własny model) bez narzędzi. `None` = brak mózgu / błąd.
async fn ask_brain(shared: &Arc<Shared>, system: &str, prompt: &str, max_chars: usize) -> Option<String> {
    let config = shared.config();
    let gemini_key = secrets::load(Provider::Gemini).await;
    let text = match resolve_mind(&config, gemini_key).await.ok()? {
        Mind::Gemini { key, model } => tokio::time::timeout(Duration::from_secs(40), gemini::generate_text(&shared.http, &key, &model, system, prompt))
            .await
            .ok()?
            .map_err(|e| tracing::warn!("LimboAI: {e:#}"))
            .ok()?,
        Mind::OpenAi { base, key, model } => {
            let ep = openai::Endpoint { base: &base, key: key.as_deref(), model: &model };
            tokio::time::timeout(Duration::from_secs(60), openai::complete(&shared.http, &ep, system, prompt))
                .await
                .ok()?
                .map_err(|e| tracing::warn!("LimboAI: {e:#}"))
                .ok()?
        }
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(max_chars).collect())
}

async fn rhyme(shared: &Arc<Shared>, nick: &str, event: &str) -> Option<String> {
    use std::sync::atomic::Ordering::SeqCst;
    // Gift-bomby i raidy potrafią przyjść falą — nie zalewamy API.
    if shared.rhyme_waiting.fetch_add(1, SeqCst) >= 5 {
        shared.rhyme_waiting.fetch_sub(1, SeqCst);
        return None;
    }
    let result = async {
        let mut last = shared.rhyme_gate.lock().await;
        if let Some(t) = *last {
            let gap = Duration::from_millis(1500);
            if t.elapsed() < gap {
                tokio::time::sleep(gap - t.elapsed()).await;
            }
        }
        *last = Some(Instant::now());
        let nick: String = nick.chars().filter(|c| c.is_alphanumeric() || *c == '_').take(25).collect();
        let prompt = format!("Nick widza: {nick}\nCo zrobił(a): {event}");
        let config = shared.config();
        let gemini_key = secrets::load(Provider::Gemini).await;
        let text = match resolve_mind(&config, gemini_key).await {
            Ok(Mind::Gemini { key, model }) => {
                tokio::time::timeout(Duration::from_secs(15), gemini::generate_text(&shared.http, &key, &model, RHYME_SYSTEM, &prompt))
                    .await
                    .ok()?
                    .map_err(|e| tracing::warn!("rymowanka: {e:#}"))
                    .ok()?
            }
            Ok(Mind::OpenAi { base, key, model }) => {
                let ep = openai::Endpoint { base: &base, key: key.as_deref(), model: &model };
                tokio::time::timeout(Duration::from_secs(20), openai::complete(&shared.http, &ep, RHYME_SYSTEM, &prompt))
                    .await
                    .ok()?
                    .map_err(|e| tracing::warn!("rymowanka: {e:#}"))
                    .ok()?
            }
            Err(_) if shared.demo => demo_rhyme(&nick),
            Err(_) => return None,
        };
        tidy_rhyme(&text)
    }
    .await;
    shared.rhyme_waiting.fetch_sub(1, SeqCst);
    result
}

/// Dwa wersy, bez cudzysłowów i ozdobników, rozsądna długość.
fn tidy_rhyme(text: &str) -> Option<String> {
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim().trim_matches(|c: char| matches!(c, '"' | '„' | '”' | '“' | '*' | '-' | '•' | '\'')).trim().to_owned())
        .filter(|l| !l.is_empty())
        .take(2)
        .collect();
    let joined = lines.join("\n");
    let clean: String = joined.chars().filter(|c| c.len_utf8() < 4).collect();
    (!clean.trim().is_empty()).then(|| clean.chars().take(220).collect())
}

fn demo_rhyme(nick: &str) -> String {
    const T: [&str; 4] = [
        "{n} wpada jak burza z prezentem w dłoni,\ncały czat dziś z wdzięczności głośno dzwoni!",
        "Dzięki, {n}, jesteś mistrzem nad mistrze,\nprzez ciebie ten stream jest jeszcze wyższe!",
        "Kto tak hojny jest o tej porze?\nTo {n} — niech mu szczęście dziś pomoże!",
        "{n} wsparcie śle prosto z serca,\nczat szaleje, a streamer się wzrusza!",
    ];
    let i = nick.bytes().map(usize::from).sum::<usize>() % T.len();
    T[i].replace("{n}", nick)
}

// ── Przypomnienia i wzmianki ───────────────────────────────────────────

fn handle_reminder(shared: &Arc<Shared>, cmd: tools::ReminderCmd) {
    match cmd {
        tools::ReminderCmd::CancelAll => {
            shared.reminder_generation.fetch_add(1, Ordering::SeqCst);
        }
        tools::ReminderCmd::Add { after, text } => {
            let generation = shared.reminder_generation.load(Ordering::SeqCst);
            let shared = shared.clone();
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                // Nie przerywamy trwającej rozmowy — czekamy do 30 s, aż Jarvis skończy.
                for _ in 0..60 {
                    if shared.phase() == Phase::Idle {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                if shared.reminder_generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                shared.voice.chime(Chime::Listen);
                shared.emit(JarvisEvent::Reminder { text: text.clone() });
                shared.notice(None, format!("⏰ Przypomnienie: {text}"), false);
                let speech = format!("Przypomnienie: {text}");
                shared.hold_wake(Duration::from_secs_f32(Voice::estimate_seconds(&speech) + 1.0));
                speak_later(&shared, speech);
            });
        }
    }
}

fn read_mention(shared: &Arc<Shared>, name: &str, text: &str) {
    let config = shared.config();
    if !config.enabled || !config.read_mentions || !config.voice || shared.phase() != Phase::Idle || shared.enrolling.load(Ordering::Relaxed) {
        return;
    }
    {
        let mut last = shared.last_mention.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        *last = Some(Instant::now());
    }
    let text: String = text.chars().take(160).collect();
    let speech = format!("{name} pisze: {text}");
    shared.hold_wake(Duration::from_secs_f32(Voice::estimate_seconds(&speech) + 1.0));
    speak_later(shared, speech);
}

// ── Głos ───────────────────────────────────────────────────────────────

/// Mówi tekst: naturalnym głosem Gemini (gdy wybrany i dostępny), inaczej głosem systemowym.
/// Zwraca czas wypowiedzi liczony od chwili, gdy zaczyna być słyszana.
async fn speak(shared: &Arc<Shared>, text: &str) -> Duration {
    let estimate = Duration::from_secs_f32(Voice::estimate_seconds(text));
    let config = shared.config();
    let paused = shared.tts_pause.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|until| Instant::now() < until);
    if config.voice_engine != "gemini" || shared.demo || paused {
        shared.voice.say(text);
        return estimate;
    }
    let ticket = shared.voice.claim();
    let Some(key) = secrets::load(Provider::Gemini).await else {
        shared.voice.say_if(ticket, text);
        return estimate;
    };
    let failure = match tokio::time::timeout(TTS_TIMEOUT, tts::synthesize(&shared.http, &key, &config.tts_voice, text)).await {
        Ok(Ok(speech)) => {
            // Uciszony w międzyczasie (np. nowa komenda) — nic nie gra, nie ma na co czekać.
            return if shared.voice.play_speech(ticket, &speech) { speech.duration() } else { Duration::ZERO };
        }
        Ok(Err(e)) => format!("{e:#}"),
        Err(_) => "brak odpowiedzi w czasie".to_owned(),
    };
    tracing::warn!("głos Gemini niedostępny ({failure}) — przez kilka minut mówi głos systemowy");
    *shared.tts_pause.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + TTS_PAUSE);
    shared.voice.say_if(ticket, text);
    estimate
}

/// `speak` w tle (przypomnienia, wzmianki, „przeczytaj”) — pauza „Hej Limbo” na czas mówienia.
fn speak_later(shared: &Arc<Shared>, text: String) {
    let shared = shared.clone();
    tokio::spawn(async move {
        let length = speak(&shared, &text).await;
        shared.hold_wake(length + Duration::from_secs(1));
    });
}

/// Krótki opis streamu do kontekstu (tytuł, kategoria) — pobierany w tle.
async fn stream_summary(shared: &Arc<Shared>, ctx: &tools::Context) -> Option<(String, String)> {
    if ctx.demo {
        return Some(("Wieczorne granie z czatem".into(), "Just Chatting".into()));
    }
    let (_, own_id) = ctx.me.as_ref()?;
    let info = shared.helix.as_ref()?.channel_info(own_id).await.ok()??;
    Some((info.title, info.game_name))
}

async fn think(
    shared: &Arc<Shared>,
    ctx: &tools::Context,
    stream: &Option<(String, String)>,
    transcript: &str,
    mind: &Mind,
) -> Result<(String, Vec<tools::Action>), String> {
    let mut brain = shared.brain.lock().await;
    if brain.kind != mind.kind() || brain.last.is_none_or(|t| t.elapsed() > MEMORY) || brain.history.len() > MAX_HISTORY {
        brain.history.clear();
        brain.kind = mind.kind();
    }
    brain.last = Some(Instant::now());
    brain.history.push(match mind {
        Mind::Gemini { .. } => gemini::user_text(transcript),
        Mind::OpenAi { .. } => openai::user_text(transcript),
    });

    let system = system_prompt(ctx, stream, brain.executor.pending_label());
    let declarations = tools::declarations();
    let openai_tools = matches!(mind, Mind::OpenAi { .. }).then(|| openai::tools(&declarations));
    let mut actions = Vec::new();

    for _ in 0..MAX_ROUNDS {
        let reply = match mind {
            Mind::Gemini { key, model } => gemini::generate(&shared.http, key, model, &system, &brain.history, &declarations)
                .await
                .map(|c| (gemini::function_calls(&c), gemini::text(&c), c)),
            Mind::OpenAi { base, key, model } => {
                let ep = openai::Endpoint { base, key: key.as_deref(), model };
                openai::chat(&shared.http, &ep, &system, &brain.history, openai_tools.as_ref().unwrap_or(&Value::Null))
                    .await
                    .map(|m| (openai::tool_calls(&m), openai::text(&m), m))
            }
        };
        let (calls, text, message) = match reply {
            Ok(r) => r,
            Err(e) => {
                // Niepełna tura (wywołanie bez odpowiedzi) zepsułaby historię.
                brain.history.clear();
                return Err(format!("{e:#}"));
            }
        };
        brain.history.push(message);
        if calls.is_empty() {
            brain.last = Some(Instant::now());
            return Ok((clean_reply(&text), actions));
        }
        let mut results = Vec::new();
        for call in calls {
            let outcome = brain.executor.run(ctx, &call.name, &call.args).await;
            if let Some(action) = outcome.action {
                shared.emit(JarvisEvent::Action { text: action.text.clone(), ok: action.ok, url: action.url.clone() });
                actions.push(action);
            }
            results.push((call, outcome.result));
        }
        match mind {
            Mind::Gemini { .. } => brain.history.push(gemini::function_responses(&results)),
            Mind::OpenAi { .. } => brain.history.extend(openai::tool_results(&results)),
        }
    }
    brain.history.clear();
    Ok((String::new(), actions))
}

/// Tryb demo bez klucza: pokazowy przebieg (do obejrzenia interfejsu).
async fn scripted_reply(shared: &Arc<Shared>, ctx: &tools::Context) -> (String, Vec<tools::Action>) {
    tokio::time::sleep(Duration::from_millis(600)).await;
    let mut brain = shared.brain.lock().await;
    let outcome = brain.executor.run(ctx, "set_category", &serde_json::json!({ "name": "Grand Theft Auto IV" })).await;
    let actions: Vec<_> = outcome.action.into_iter().collect();
    for a in &actions {
        shared.emit(JarvisEvent::Action { text: a.text.clone(), ok: a.ok, url: a.url.clone() });
    }
    ("Gotowe, kategoria zmieniona na Grand Theft Auto IV. Powodzenia w Liberty City.".into(), actions)
}

/// Odpowiedź idzie do syntezatora mowy — bez markdownu, emoji i linków.
fn clean_reply(text: &str) -> String {
    // Emoji (znaki 4-bajtowe) i znaczniki markdownu wycinamy, linki pomijamy.
    let plain: String = text.chars().filter(|c| !matches!(c, '*' | '_' | '#' | '`') && c.len_utf8() < 4).collect();
    plain.split_whitespace().filter(|w| !w.starts_with("http")).collect::<Vec<_>>().join(" ")
}

fn system_prompt(ctx: &tools::Context, stream: &Option<(String, String)>, pending: Option<String>) -> String {
    let now = chrono::Local::now().format("%d.%m.%Y, %H:%M");
    let me = ctx.me.as_ref().map_or("(niezalogowany)".to_owned(), |(login, _)| login.clone());
    let active = ctx.active.as_ref().map_or("(brak — moderacja na Twoim kanale)".to_owned(), |c| {
        format!("#{} (uprawnienia moderatora: {})", c.login, if ctx.can_moderate_active { "tak" } else { "nie" })
    });
    let chatters = if ctx.chatters.is_empty() {
        "(nikt jeszcze nie pisał)".to_owned()
    } else {
        ctx.chatters.iter().take(60).map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
    };
    let stream = stream.as_ref().map_or("(nieznane)".to_owned(), |(title, game)| format!("tytuł „{title}”, kategoria {game}"));
    let pending = pending.map_or(String::new(), |p| format!("\n- CZEKA NA POTWIERDZENIE: {p}. Jeśli użytkownik potwierdza lub odmawia — wywołaj confirm_pending."));
    let obs = match &ctx.obs {
        Some(o) if o.connected => format!(
            "\n- OBS: połączony. Sceny: {}. Aktualna: {}. Stream: {}. Nagrywanie: {}.",
            o.scenes.join(", "),
            o.current_scene.as_deref().unwrap_or("?"),
            if o.streaming { "trwa" } else { "nie" },
            if o.recording { "trwa" } else { "nie" }
        ),
        _ => "\n- OBS: brak połączenia (sterowanie OBS niedostępne — powiedz, jak je włączyć: ⚙ → OBS).".to_owned(),
    };
    let memory = if ctx.memory.is_empty() {
        String::new()
    } else {
        format!("\n- Co pamiętasz o osobach z czatu: {}", ctx.memory.iter().map(|(n, b)| format!("{n}: {b}")).collect::<Vec<_>>().join(" | "))
    };

    format!(
        "Jesteś LimboAI — głosowy asystent streamera Twitch, wbudowany w OBS (LiMBOCHAT). \
Mówisz po polsku, krótko (1–2 zdania, do ~25 słów), naturalnie, z lekką nutą kamerdynera z Iron Mana — bez przesady. \
Twoje odpowiedzi czyta syntezator mowy: żadnych emoji, markdownu, list ani adresów URL.

Zasady:
- Polecenia wykonuj narzędziami. Nie mów, że coś zrobiłeś, jeśli narzędzie zwróciło błąd — powiedz krótko, co poszło nie tak.
- Kategorie podawaj jako pełną oficjalną nazwę z Twitcha, np. „GTA 4” → „Grand Theft Auto IV”, „GTA 5”/„piątka” → „Grand Theft Auto V”, „lol” → „League of Legends”, „cs” → „Counter-Strike”, „pogaduchy”/„just chatting” → „Just Chatting”.
- Rozpoznawanie mowy przekręca nicki: użytkownika wybieraj z listy osób z czatu poniżej (najbliższy pasujący). Gdy nie masz pewności, dopytaj.
- Czas przeliczaj na sekundy (10 minut = 600). Timeout bez podanego czasu: 10 minut.
- Ban, wyczyszczenie czatu, raid, reklama i nadanie moderatora wymagają potwierdzenia: wywołaj narzędzie, a gdy zwróci needs_confirmation — zapytaj krótko „Na pewno…?”. Gdy użytkownik potem potwierdzi („tak”, „potwierdzam”, „dawaj”) wywołaj confirm_pending(true), gdy odmówi — confirm_pending(false).
- Pytania o stream (widzowie, czas, tytuł) → get_stream_info. O czat (co piszą, kto spamuje) → get_recent_chat.
- Kolor panelu LiMBOCHAT → set_panel_color; kolor nicku na czacie → set_nick_color (to dwie różne rzeczy).
- „Przypomnij mi za…” → set_reminder. „Wylosuj kogoś z czatu” → pick_random_chatter i przeczytaj zwycięzcę.
- Łagodne upomnienie → warn_user; nalot hejtu/botów → set_shield_mode.
- OBS: scena → obs_switch_scene (nazwę dopasuj do listy scen), mikrofon/dźwięk → obs_set_mute, pokaż/schowaj kamerkę lub inne źródło → obs_set_source_visible, nagrywanie → obs_recording, „zapisz powtórkę/to było dobre” → obs_save_replay. Start i koniec streama (obs_streaming) wymagają potwierdzenia.
- Pamięć o widzach: „zapamiętaj, że X…” → remember_viewer; „zapomnij…” → forget_viewer; „co wiesz o X?” / „kto to X?” → viewer_info. Korzystaj z tego, co pamiętasz, ale nie zdradzaj prywatnych notatek na głos bez pytania.
- „Jakie są wyniki ankiety?”, „kto wygrał ankietę?” → poll_results; o predykcję → prediction_results. Działa też po zakończeniu — zawsze sprawdź narzędziem, zanim powiesz, że ankiety nie było. Podaj zwycięzcę i procenty.
- „Zapamiętaj ten moment”, „zapisz znacznik”, „ale była akcja” → mark_moment. To znacznik do montażu, nie notatka o widzu. „Zapisz powtórkę” → obs_save_replay; zapis jest potwierdzony dopiero zdarzeniem OBS.
- Użytkownik woła Cię „LimboAI”, „Limbo”, „Hej Limbo” albo „Jarvis” — to imiona dla Ciebie, nie komendy.
- Twoje odpowiedzi mogą być widoczne na streamie (overlay) — bądź kulturalny.
- Jeśli wypowiedź nie ma sensu (błąd rozpoznawania), poproś o powtórzenie.

Kontekst:
- Teraz: {now}
- Twój kanał (zalogowany streamer): {me} — tu zmieniasz kategorię, tytuł, robisz klipy, markery, raidy, ankiety i reklamy.
- Kanał otwarty w panelu (moderacja czatu, wiadomości): {active}
- Stream: {stream}
- Osoby na czacie (ostatnio piszące): {chatters}{obs}{memory}{pending}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_speakable() {
        assert_eq!(clean_reply("**Gotowe!** 🎮 Zobacz https://twitch.tv/x"), "Gotowe! Zobacz");
    }

    #[test]
    fn default_config_uses_requested_hotkey() {
        let c = JarvisConfig::default();
        assert_eq!(c.hotkey, ["Shift", "Z", "X"]);
        assert_eq!(active_combo(&c), vec![0x10, 0x5A, 0x58]);
        assert!(active_combo(&JarvisConfig { enabled: false, ..c }).is_empty());
    }

    #[test]
    fn panel_sends_full_settings() {
        let msg: crate::protocol::ClientMsg = serde_json::from_str(
            r#"{"c":"jarvis_settings","enabled":true,"voice":true,"hotkey":["Mysz4"],"brain":"custom","llm_preset":"ollama","llm_model":"qwen2.5:7b","talk_mode":"hold"}"#,
        )
        .unwrap();
        let crate::protocol::ClientMsg::JarvisSettings(c) = msg else { panic!("zły typ") };
        assert_eq!((c.brain.as_str(), c.llm_preset.as_str(), c.talk_mode.as_str()), ("custom", "ollama", "hold"));
        assert_eq!(c.wake_sensitivity, 2, "brakujące pola = domyślne");
        let overlay: crate::protocol::ClientMsg = serde_json::from_str(r#"{"c":"overlay","position":"top-right","scale":500}"#).unwrap();
        let crate::protocol::ClientMsg::Overlay(o) = overlay else { panic!() };
        let o = o.sanitized();
        assert_eq!((o.position.as_str(), o.scale, o.show_command), ("top-right", 160, true));
    }

    #[tokio::test]
    async fn custom_brain_requirements() {
        let mut c = JarvisConfig { brain: "custom".into(), llm_preset: "ollama".into(), llm_model: "qwen2.5:7b".into(), ..Default::default() };
        assert!(matches!(resolve_mind(&c, None).await, Ok(Mind::OpenAi { key: None, .. })), "Ollama bez klucza");
        c.llm_model.clear();
        assert!(resolve_mind(&c, None).await.is_err(), "bez nazwy modelu");
        c = JarvisConfig { brain: "custom".into(), llm_preset: "custom".into(), llm_url: "http://example.com/v1".into(), llm_model: "x".into(), ..Default::default() };
        assert!(resolve_mind(&c, None).await.is_err(), "zwykłe http poza komputerem");
        c.brain = "gemini".into();
        assert!(matches!(resolve_mind(&c, Some("k".into())).await, Ok(Mind::Gemini { .. })));
    }

    #[test]
    fn rhymes_are_tidied() {
        assert_eq!(tidy_rhyme("„Gaduła sypie bitsy jak z rękawa,\n- czat się cieszy, jest zabawa!”\nTo wszystko!").as_deref(), Some("Gaduła sypie bitsy jak z rękawa,\nczat się cieszy, jest zabawa!"));
        assert_eq!(tidy_rhyme("  \n "), None);
        assert!(demo_rhyme("Gaduła").contains("Gaduła"));
    }

    #[test]
    fn old_settings_files_get_new_defaults() {
        let c: JarvisConfig = serde_json::from_str(r#"{ "enabled": true, "voice": false }"#).unwrap();
        assert!(!c.voice && !c.wake_word && c.wake_sensitivity == 2 && !c.read_mentions);
    }

    #[test]
    fn system_prompt_mentions_pending_confirmation() {
        let ctx = tools::Context { me: Some(("limbo".into(), "1".into())), ..Default::default() };
        let p = system_prompt(&ctx, &None, Some("ban dla Spamer".into()));
        assert!(p.contains("CZEKA NA POTWIERDZENIE: ban dla Spamer"));
        assert!(p.contains("limbo"));
    }
}
