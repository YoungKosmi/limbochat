//! Protokół JSON między wtyczką a panelem (przez WebSocket).
//!
//! * Serwer → panel: `ServerMsg` (pole `t` = typ zdarzenia).
//! * Panel → serwer: `ClientMsg` (pole `c` = komenda).
//!
//! Panel identyfikuje kanały po loginie; ID kanału (potrzebne Helix)
//! uzupełnia serwer — panel nie musi go znać ani mu ufać.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    config::Config,
    jarvis::{JarvisEvent, JarvisStatus},
    emotes::{Emote, EmoteProvider},
    helix::BadgeMap,
    parser::{ParsedMessage, Segment},
};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Snapshot(Snapshot),
    Message { m: MessageDto },
    /// Pełna historia kanału (np. po załadowaniu emotek 7TV).
    History { channel: String, messages: Vec<MessageDto> },
    ChannelAdded { channel: String },
    ChannelRemoved { channel: String },
    Deleted { channel: String, id: String },
    UserCleared { channel: String, user_id: String, login: String, timeout: Option<u64> },
    ChatCleared { channel: String },
    Connected { channel: String },
    Auth(AuthDto),
    Avatar { url: String },
    ModStatus { channel: String, can_moderate: bool },
    Badges { channel: Option<String>, map: HashMap<String, BadgeDto> },
    EmotesChanged { channel: Option<String>, count: usize },
    EmoteList { channel: String, emotes: Vec<EmoteDto> },
    Notice { channel: Option<String>, text: String, error: bool },
    Settings(Config),
    Update(crate::update::UpdateStatus),
    /// Zmiana istniejącego alertu (rymowanka gotowa, kolejni obdarowani w gift-bombie).
    AlertUpdate { channel: String, id: String, alert: AlertDto },
    Obs(crate::obs::ObsStatus),
    Studio { event:String, data:serde_json::Value },
    /// Podsumowanie streama (nowe albo z dopisanym tekstem LimboAI).
    Summary(crate::summary::Summary),
    Summaries { list: Vec<crate::summary::Summary> },
    Changelog { entries: Vec<crate::changelog::Entry> },
    /// Pamięć o widzu (karta widza w panelu).
    Memory { login: String, viewer: Option<crate::memory::Viewer>, count: usize },
    Jarvis { ev: JarvisEvent },
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub auth: AuthDto,
    pub avatar: Option<String>,
    pub settings: Config,
    pub global_badges: HashMap<String, BadgeDto>,
    pub global_emotes: usize,
    pub channels: Vec<ChannelDto>,
    pub demo: bool,
    pub jarvis: JarvisStatus,
    /// Wersja silnika — panel przeładowuje się, gdy po aktualizacji jest inna.
    pub version: String,
    pub update: crate::update::UpdateStatus,
    pub obs: crate::obs::ObsStatus,
    /// „Co nowego” — tylko zaraz po aktualizacji.
    pub changelog: Vec<crate::changelog::Entry>,
    /// Ostatnie podsumowanie streama (jeśli było w tej sesji).
    pub summary: Option<crate::summary::Summary>,
    pub memory_count: usize,
    pub studio: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelDto {
    pub login: String,
    pub connected: bool,
    pub can_moderate: bool,
    pub emote_count: usize,
    pub badges: HashMap<String, BadgeDto>,
    pub messages: Vec<MessageDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AuthDto {
    Restoring,
    LoggedOut,
    Pending { code: String, uri: String, expires_in: u64 },
    LoggedIn { login: String, user_id: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct BadgeDto {
    pub url: String,
    pub title: String,
}

pub fn badges_dto(map: &BadgeMap) -> HashMap<String, BadgeDto> {
    map.iter()
        .map(|(k, v)| (k.clone(), BadgeDto { url: v.url.to_string(), title: v.title.clone() }))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmoteDto {
    pub name: String,
    pub url: String,
    pub w: f32,
    pub h: f32,
    pub zw: bool,
    pub src: String,
}

impl From<&Emote> for EmoteDto {
    fn from(e: &Emote) -> Self {
        Self {
            name: e.name.to_string(),
            url: e.url.to_string(),
            w: e.width,
            h: e.height,
            zw: e.zero_width,
            src: match e.provider {
                EmoteProvider::Twitch => "twitch",
                EmoteProvider::SevenTv => "7tv",
            }
            .to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum SegmentDto {
    Text { v: String },
    Emote { e: EmoteDto, ov: Vec<EmoteDto> },
    Mention { v: String },
    Link { v: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDto {
    pub id: String,
    pub channel: String,
    pub user_id: String,
    pub login: String,
    pub name: String,
    pub color: Option<String>,
    pub badges: Vec<(String, String)>,
    pub action: bool,
    /// Znacznik czasu w milisekundach (UTC).
    pub ts: i64,
    pub deleted: bool,
    pub raw: String,
    pub segs: Vec<SegmentDto>,
    /// Ustawione dla alertów (bitsy, suby, raidy…) — panel rysuje kartę.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert: Option<AlertDto>,
}

/// Dane alertu dla panelu (teksty po polsku składa panel).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlertDto {
    /// bits / sub / resub / gift / mass_gift / raid / bits_badge / upgrade / new_chatter / follow / redemption
    pub kind: String,
    /// Bitsy, miesiące, liczba gifów, widzowie raidu, koszt nagrody…
    #[serde(default)]
    pub amount: u64,
    #[serde(default)]
    pub tier: Option<String>,
    /// Obdarowany / darczyńca / nazwa nagrody.
    #[serde(default)]
    pub target: Option<String>,
    /// Tekst wpisany przy nagrodzie albo lista obdarowanych przy gift-bombie.
    #[serde(default)]
    pub extra: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub anonymous: bool,
    /// Rymowanka LimboAI (przychodzi chwilę później).
    #[serde(default)]
    pub rhyme: Option<String>,
    #[serde(default)]
    pub rhyme_pending: bool,
    /// Alert próbny z ustawień (nie idzie na czat Twitcha).
    #[serde(default)]
    pub test: bool,
}

impl From<&ParsedMessage> for MessageDto {
    fn from(m: &ParsedMessage) -> Self {
        Self {
            id: m.id.clone(),
            channel: m.channel_login.clone(),
            user_id: m.sender_id.clone(),
            login: m.sender_login.clone(),
            name: m.sender_name.clone(),
            color: m.color.map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}")),
            badges: m.badges.iter().map(|b| (b.name.clone(), b.version.clone())).collect(),
            action: m.is_action,
            ts: m.timestamp.timestamp_millis(),
            deleted: m.deleted,
            raw: m.raw_text.clone(),
            segs: m
                .segments
                .iter()
                .map(|s| match s {
                    Segment::Text(v) => SegmentDto::Text { v: v.clone() },
                    Segment::Emote(e) => SegmentDto::Emote {
                        e: EmoteDto::from(e.base.as_ref()),
                        ov: e.overlays.iter().map(|o| EmoteDto::from(o.as_ref())).collect(),
                    },
                    Segment::Mention(v) => SegmentDto::Mention { v: v.clone() },
                    Segment::Link(v) => SegmentDto::Link { v: v.clone() },
                })
                .collect(),
            alert: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "c", rename_all = "snake_case")]
pub enum ClientMsg {
    Join { channel: String },
    Part { channel: String },
    Activate { channel: String },
    Login,
    CancelLogin,
    Logout,
    Send { channel: String, text: String },
    Timeout { channel: String, user_id: String, login: String, seconds: u32 },
    Ban { channel: String, user_id: String, login: String },
    Unban { channel: String, user_id: String, login: String },
    Delete { channel: String, message_id: String },
    Emotes { channel: String },
    /// Otwórz adres w systemowej przeglądarce (link z czatu, profil).
    Open { url: String },
    Settings { animations: bool, timestamps: bool, mod_buttons: bool },
    Theme { accent: String },
    JarvisActivate,
    JarvisRefresh,
    /// `key: None` usuwa klucz. Klucz nigdy nie wraca do panelu.
    JarvisKey { provider: String, key: Option<String> },
    /// Pełne ustawienia asystenta (brakujące pola = domyślne).
    JarvisSettings(crate::jarvis::JarvisConfig),
    /// Pobierz listę modeli od dostawcy własnego modelu.
    JarvisModels,
    /// Ustawienia alertów i rymowanek.
    Alerts(crate::config::AlertsConfig),
    /// Podgląd obrazków z linków: all / trusted / off.
    MediaPreviews { mode: String },
    ObsSettings(crate::obs::ObsConfig),
    Studio { command:crate::studio::Command },
    /// `None` usuwa hasło. Hasło nigdy nie wraca do panelu.
    ObsPassword { password: Option<String> },
    MemorySettings(crate::config::MemoryConfig),
    MemoryGet { login: String },
    MemoryAdd { login: String, name: String, text: String },
    MemoryForget { login: String, index: usize },
    MemoryClear,
    SummarySettings(crate::config::SummaryConfig),
    /// Podsumuj teraz (bez czekania na koniec streama).
    SummaryNow,
    SummaryHistory,
    Changelog,
    /// Powitania VIP-ów.
    Greetings(crate::config::GreetingsConfig),
    /// Przykładowy alert z ustawień (tylko w panelu).
    AlertTest { kind: String },
    /// Aktualizacje: sprawdź teraz / zainstaluj teraz / tryb (auto | ask).
    UpdateCheck,
    UpdateApply,
    UpdateMode { mode: String },
    /// Ustawienia overlayu na stream.
    Overlay(crate::config::OverlayConfig),
    /// Nauka „Hej Limbo”: start / cancel / reset.
    JarvisEnroll { action: String },
}

