//! Klucze API (Gemini, Groq, własny model) w sejfie systemowym (Windows Credential Manager).
//!
//! Panel może klucz zapisać lub usunąć, ale nigdy go nie odczytuje — dostaje
//! tylko informację „zapisany / brak”. Klucz nie trafia do pliku ustawień
//! ani do logów.
//!
//! Do testów można podać klucze w zmiennych środowiskowych
//! `LIMBOCHAT_GEMINI_KEY` / `LIMBOCHAT_GROQ_KEY` (mają pierwszeństwo).

use anyhow::Context;

const SERVICE: &str = "limbochat";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Gemini,
    Groq,
    /// Własny model (OpenAI, OpenRouter itd.).
    Llm,
    /// Hasło serwera WebSocket w OBS (sterowanie OBS-em).
    Obs,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gemini" => Some(Self::Gemini),
            "groq" => Some(Self::Groq),
            "llm" => Some(Self::Llm),
            "obs" => Some(Self::Obs),
            _ => None,
        }
    }

    fn account(self) -> &'static str {
        match self {
            Self::Gemini => "gemini-api-key",
            Self::Groq => "groq-api-key",
            Self::Llm => "custom-llm-api-key",
            Self::Obs => "obs-websocket-password",
        }
    }

    fn env(self) -> &'static str {
        match self {
            Self::Gemini => "LIMBOCHAT_GEMINI_KEY",
            Self::Groq => "LIMBOCHAT_GROQ_KEY",
            Self::Llm => "LIMBOCHAT_LLM_KEY",
            Self::Obs => "LIMBOCHAT_OBS_PASSWORD",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Gemini => "Gemini",
            Self::Groq => "Groq",
            Self::Llm => "własnego modelu",
            Self::Obs => "OBS",
        }
    }
}

pub async fn load(provider: Provider) -> Option<String> {
    if let Ok(key) = std::env::var(provider.env()) {
        if !key.trim().is_empty() {
            return Some(key.trim().to_owned());
        }
    }
    tokio::task::spawn_blocking(move || {
        let entry = keyring::Entry::new(SERVICE, provider.account()).ok()?;
        entry.get_password().ok().filter(|k| !k.trim().is_empty())
    })
    .await
    .ok()
    .flatten()
}

/// `None` usuwa klucz.
pub async fn store(provider: Provider, key: Option<String>) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let entry = keyring::Entry::new(SERVICE, provider.account())?;
        match key.map(|k| k.trim().to_owned()).filter(|k| !k.is_empty()) {
            Some(key) => entry.set_password(&key).context("zapis do sejfu systemowego")?,
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(e.into()),
            },
        }
        Ok(())
    })
    .await?
}
