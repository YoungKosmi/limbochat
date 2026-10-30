//! Licznik zapytań LimboAI do usług AI — „ile dziś zużyliśmy”.
//!
//! Dzień liczymy jak Google: od północy czasu pacyficznego (UTC−8, czyli ok. 9:00
//! w Polsce; przy czasie letnim w USA godzinę wcześniej). Liczymy każde zapytanie,
//! na które przyszła odpowiedź (także błąd); `limited` = odpowiedzi „limit wyczerpany” (HTTP 429).
//! Stan zapisujemy w `ai-usage.json` w folderze danych, żeby przetrwał aktualizację silnika.

use std::sync::Mutex;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    /// Mózg Gemini (komendy, rymowanki, podsumowania, pamięć).
    Gemini,
    /// Głos Gemini (TTS).
    Voice,
    /// Rozpoznawanie mowy przez Gemini (zapas, „Hej Limbo”).
    Listen,
    /// Rozpoznawanie mowy przez Groq (Whisper).
    Groq,
    /// Własny model (OpenAI, OpenRouter, Groq, Ollama…).
    Custom,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Usage {
    /// Dzień Google `RRRR-MM-DD`.
    pub day: String,
    pub gemini: u32,
    pub voice: u32,
    pub listen: u32,
    pub groq: u32,
    pub custom: u32,
    pub limited: u32,
}

static STATE: Mutex<Option<Usage>> = Mutex::new(None);

fn google_day(now: DateTime<Utc>) -> String {
    (now - TimeDelta::hours(8)).format("%Y-%m-%d").to_string()
}

fn file() -> Option<std::path::PathBuf> {
    crate::config::data_dir().map(|d| d.join("ai-usage.json"))
}

fn load() -> Usage {
    file().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Nowy dzień = liczniki od zera.
fn roll(usage: &mut Usage, today: &str) {
    if usage.day != today {
        *usage = Usage { day: today.to_owned(), ..Usage::default() };
    }
}

fn bump(usage: &mut Usage, today: &str, kind: Kind, status: u16) {
    roll(usage, today);
    let counter = match kind {
        Kind::Gemini => &mut usage.gemini,
        Kind::Voice => &mut usage.voice,
        Kind::Listen => &mut usage.listen,
        Kind::Groq => &mut usage.groq,
        Kind::Custom => &mut usage.custom,
    };
    *counter = counter.saturating_add(1);
    if status == 429 {
        usage.limited = usage.limited.saturating_add(1);
    }
}

/// Zapytanie, na które przyszła odpowiedź z kodem HTTP `status`.
pub fn record(kind: Kind, status: u16) {
    let today = google_day(Utc::now());
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let usage = state.get_or_insert_with(load);
    bump(usage, &today, kind, status);
    if let (Some(path), Ok(json)) = (file(), serde_json::to_string(usage)) {
        if let Err(e) = std::fs::write(&path, json) {
            tracing::debug!("nie zapisano licznika AI ({}): {e}", path.display());
        }
    }
}

/// Liczniki na dziś (do ustawień panelu).
pub fn today() -> Usage {
    let today = google_day(Utc::now());
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let usage = state.get_or_insert_with(load);
    roll(usage, &today);
    usage.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_starts_at_pacific_midnight() {
        let before = DateTime::parse_from_rfc3339("2026-10-31T07:59:00Z").unwrap().with_timezone(&Utc);
        let after = DateTime::parse_from_rfc3339("2026-10-31T08:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(google_day(before), "2026-10-30");
        assert_eq!(google_day(after), "2026-10-31");
    }

    #[test]
    fn counts_per_kind_and_resets_next_day() {
        let mut u = Usage::default();
        bump(&mut u, "2026-10-30", Kind::Gemini, 200);
        bump(&mut u, "2026-10-30", Kind::Gemini, 429);
        bump(&mut u, "2026-10-30", Kind::Voice, 200);
        assert_eq!((u.gemini, u.voice, u.limited), (2, 1, 1));
        bump(&mut u, "2026-10-31", Kind::Groq, 200);
        assert_eq!(u, Usage { day: "2026-10-31".into(), groq: 1, ..Usage::default() });
    }
}
