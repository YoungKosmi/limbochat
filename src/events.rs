//! Kontrakt komunikacji między runtime'em Tokio a wątkiem UI.
//!
//! * `UiEvent`  — płynie z backendu do GUI (wiadomości, usunięcia, statusy).
//! * `Command`  — płynie z GUI do backendu (kanały, logowanie, czat, moderacja).
//!
//! Aplikacja obsługuje wiele kanałów naraz, więc prawie każde zdarzenie
//! niesie login kanału (`channel`) — UI kieruje je do właściwej zakładki.

use std::{sync::Arc, time::Instant};

use tokio::sync::mpsc;

use crate::{helix::BadgeMap, parser::ParsedMessage};

#[derive(Debug)]
pub enum UiEvent {
    /// Dołączono do kanału i otrzymano ROOMSTATE (znamy już `channel_id`).
    Connected { channel: String, channel_id: String },
    /// Nowa, już sparsowana wiadomość (kanał jest w `ParsedMessage::channel_login`).
    Message(Box<ParsedMessage>),
    /// CLEARMSG — pojedyncza wiadomość usunięta (przez nas lub innego moda).
    MessageDeleted { channel: String, message_id: String },
    /// CLEARCHAT dla użytkownika: ban (`timeout_secs == None`) lub timeout.
    UserCleared { channel: String, user_id: String, login: String, timeout_secs: Option<u64> },
    /// CLEARCHAT bez użytkownika — wyczyszczony cały czat kanału.
    ChatCleared { channel: String },
    /// `channel == None` — zmieniły się emotki globalne (dotyczy wszystkich zakładek).
    EmotesLoaded { channel: Option<String>, count: usize },
    Auth(AuthState),
    /// Adres awatara zalogowanego użytkownika.
    Avatar { url: String },
    /// Czy zalogowany użytkownik może moderować dany kanał.
    ModStatus { channel: String, can_moderate: bool },
    /// `channel == None` — odznaki globalne; `Some` — kanałowe (np. subskrypcji).
    Badges { channel: Option<String>, map: Arc<BadgeMap> },
    /// `channel == None` — komunikat ogólny, pokazywany w aktywnej zakładce.
    Notice { channel: Option<String>, text: String },
    Error { channel: Option<String>, text: String },
    /// Bitsy, suby, gifty, raidy, follow, nagrody za punkty — „alerty” na czacie.
    Alert(Box<ChatAlert>),
}

/// Wyróżnione zdarzenie kanału (z IRC USERNOTICE / bitsów albo z EventSub).
#[derive(Debug, Clone)]
#[allow(dead_code)] // część pól i rodzajów używa tylko wtyczka OBS
pub struct ChatAlert {
    pub id: String,
    pub channel_login: String,
    pub user_id: String,
    pub user_login: String,
    pub user_name: String,
    pub color: Option<[u8; 3]>,
    pub kind: AlertKind,
    /// Wiadomość dołączona do zdarzenia (tekst przy bitsach / resubie).
    pub message: Option<ParsedMessage>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum AlertKind {
    Bits { amount: u64 },
    /// `tier`: "Prime", "1", "2" albo "3".
    Sub { tier: String, months: u64, resub: bool },
    GiftSub { tier: String, recipient: String, anonymous: bool },
    MassGift { tier: String, count: u64, total: Option<u64>, anonymous: bool },
    Raid { viewers: u64, avatar: String },
    BitsBadge { threshold: u64 },
    GiftUpgrade { gifter: Option<String> },
    NewChatter,
    Follow,
    Redemption { reward: String, cost: u64, input: Option<String> },
}

/// "1000" → "1", "Prime" → "Prime".
pub fn sub_tier(plan: &str) -> String {
    match plan {
        "2000" => "2".into(),
        "3000" => "3".into(),
        "Prime" | "prime" => "Prime".into(),
        _ => "1".into(),
    }
}

impl UiEvent {
    pub fn notice(text: impl Into<String>) -> Self {
        Self::Notice { channel: None, text: text.into() }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self::Error { channel: None, text: text.into() }
    }
}

#[derive(Debug, Clone)]
pub enum AuthState {
    /// Przy starcie, zanim sprawdzimy sejf.
    Restoring,
    LoggedOut,
    /// Czekamy, aż użytkownik zatwierdzi kod na twitch.tv/activate.
    Pending { user_code: String, verification_uri: String, expires_at: Instant },
    LoggedIn { login: String, user_id: String },
}

/// Kanał, którego dotyczy akcja: login (do komunikatów) i ID (do Helix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRef {
    pub login: String,
    pub id: String,
}

#[derive(Debug, Clone)]
pub enum Command {
    Join(String),
    Part(String),
    Login,
    CancelLogin,
    Logout,
    SendMessage { channel: ChannelRef, text: String },
    Timeout { channel: ChannelRef, user_id: String, login: String, seconds: u32, reason: Option<String> },
    Ban { channel: ChannelRef, user_id: String, login: String, reason: Option<String> },
    Unban { channel: ChannelRef, user_id: String, login: String },
    DeleteMessage { channel: ChannelRef, message_id: String },
    Shutdown,
}

/// Funkcja budząca UI po wysłaniu zdarzenia. W GUI to
/// `egui::Context::request_repaint` — backend nie zależy od egui.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Nadawca zdarzeń do UI, który automatycznie budzi pętlę renderowania.
#[derive(Clone)]
pub struct UiSender {
    tx: mpsc::UnboundedSender<UiEvent>,
    waker: Waker,
}

impl UiSender {
    pub fn new(tx: mpsc::UnboundedSender<UiEvent>, waker: Waker) -> Self {
        Self { tx, waker }
    }

    pub fn send(&self, event: UiEvent) {
        // Błąd oznacza tylko, że UI zostało zamknięte — nie ma kogo budzić.
        if self.tx.send(event).is_ok() {
            (self.waker)();
        }
    }
}
