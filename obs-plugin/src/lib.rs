//! Silnik LiMBOCHAT dla OBS Studio.
//!
//! ## Jak to działa
//! Wtyczka OBS (`limbochat.dll`, pakiet `obs-launcher`) jest malutka: przy
//! starcie OBS uruchamia ten silnik jako osobny proces (`limbochat-engine`),
//! a przy zamknięciu OBS go zatrzymuje. Silnik ma ten sam kod co aplikacja
//! PC (IRC, 7TV, Helix, logowanie), LimboAI oraz serwer na `127.0.0.1:47250`
//! z panelem (dock) i overlayem na stream.
//!
//! ## Dlaczego osobny proces
//! * **Aktualizacje bez restartu OBS** (moduł `update`): nowa wersja silnika
//!   pobiera się z GitHub Releases, sprawdza podpis, uruchamia obok i przejmuje
//!   port — panel i overlay łączą się ponownie same, czat zostaje.
//! * Awaria silnika nie wyłoży OBS.
//! * Wtyczka nie wywołuje żadnych funkcji libobs — nie trzeba SDK OBS ani Qt.
//!
//! ## Bezpieczeństwo
//! Serwer słucha tylko na 127.0.0.1 i odrzuca połączenia z innych stron
//! (sprawdzanie nagłówków Host i Origin). Polecenia sterujące (zatrzymaj,
//! przejmij) wymagają losowego tokenu z pliku w folderze silnika.
//! Aktualizacje są podpisane kluczem Ed25519 — bez podpisu nie zostaną użyte.

// Silnik współdzielony z aplikacją PC — te same pliki źródłowe. Część ich
// funkcji używa tylko aplikacja PC, stąd zgoda na nieużywany kod.
#![allow(dead_code)]

#[path = "../../src/auth.rs"]
mod auth;
#[path = "../../src/backend.rs"]
mod backend;
#[path = "../../src/emotes.rs"]
mod emotes;
#[path = "../../src/events.rs"]
mod events;
#[path = "../../src/helix.rs"]
mod helix;
#[path = "../../src/irc.rs"]
mod irc;
#[path = "../../src/parser.rs"]
mod parser;
#[path = "../../src/seventv.rs"]
mod seventv;

mod config;
mod demo;
mod eventsub;
pub mod engine;
mod hub;
mod jarvis;
mod media;
mod protocol;
mod server;
pub mod update;

/// Stały port — adres panelu w OBS musi się nie zmieniać.
pub const PORT: u16 = 47250;

/// Wersja silnika (testy mogą ją nadpisać przy budowaniu: `LIMBOCHAT_VERSION`).
pub fn version() -> &'static str {
    option_env!("LIMBOCHAT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}
