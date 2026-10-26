//! Klucze LimboAI dla znajomych: eksport do prywatnej paczki i import u znajomego.
//!
//! `pakiet-dla-znajomego.bat` może dołączyć do ZIP-a plik `klucze-limboai.json`
//! z kluczami streamera (Gemini, Groq, własny model) i ustawieniami mózgu.
//! `ZAINSTALUJ.bat` kopiuje go do folderu silnika jako `import-keys.json`,
//! a silnik przy starcie przenosi klucze do sejfu Windows i usuwa plik.
//!
//! Klucze NIGDY nie trafiają do publicznych wydań na GitHubie ani do kodu.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    config,
    jarvis::secrets::{self, Provider},
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SharedKeys {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groq: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<String>,
    /// Ustawienia własnego modelu (tylko gdy to model w chmurze — lokalny u znajomego nie zadziała).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brain: Option<Brain>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Brain {
    pub brain: String,
    pub llm_preset: String,
    pub llm_url: String,
    pub llm_model: String,
}

pub const IMPORT_FILE: &str = "import-keys.json";

/// `--export-keys <plik>`: zapisuje klucze z sejfu do pliku dla paczki znajomego.
pub fn cli_export(path: &Path) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut keys = rt.block_on(async {
        SharedKeys {
            gemini: secrets::load(Provider::Gemini).await,
            groq: secrets::load(Provider::Groq).await,
            llm: secrets::load(Provider::Llm).await,
            brain: None,
        }
    });
    let jarvis = config::load().jarvis;
    if jarvis.brain == "custom" && !matches!(jarvis.llm_preset.as_str(), "ollama" | "lmstudio") {
        keys.brain = Some(Brain { brain: jarvis.brain, llm_preset: jarvis.llm_preset, llm_url: jarvis.llm_url, llm_model: jarvis.llm_model });
    }
    let count = [&keys.gemini, &keys.groq, &keys.llm].iter().filter(|k| k.is_some()).count();
    if count == 0 {
        eprintln!("Nie masz zapisanych kluczy LimboAI (wklej je w panelu: ⚙ → LimboAI).");
        return 2;
    }
    match serde_json::to_vec_pretty(&keys).map_err(|e| e.to_string()).and_then(|b| std::fs::write(path, b).map_err(|e| e.to_string())) {
        Ok(()) => {
            println!("{count}");
            0
        }
        Err(e) => {
            eprintln!("Nie mogę zapisać {}: {e}", path.display());
            1
        }
    }
}

/// Przy starcie silnika: klucze z paczki znajomego → sejf Windows, plik usuwany.
pub async fn import_pending(dir: &Path) {
    let path = dir.join(IMPORT_FILE);
    let Ok(bytes) = std::fs::read(&path) else { return };
    let Ok(keys) = serde_json::from_slice::<SharedKeys>(&bytes) else {
        tracing::warn!("plik z kluczami LimboAI jest uszkodzony — usunięto");
        let _ = std::fs::remove_file(&path);
        return;
    };
    let mut imported = 0;
    let mut failed = false;
    for (provider, key) in [(Provider::Gemini, keys.gemini), (Provider::Groq, keys.groq), (Provider::Llm, keys.llm)] {
        if let Some(key) = key.filter(|k| !k.trim().is_empty()) {
            match secrets::store(provider, Some(key)).await {
                Ok(()) => imported += 1,
                Err(e) => {
                    failed = true;
                    tracing::warn!("nie udało się zapisać klucza {}: {e:#}", provider.name());
                }
            }
        }
    }
    if let Some(brain) = keys.brain {
        let mut cfg = config::load();
        cfg.jarvis.brain = brain.brain;
        cfg.jarvis.llm_preset = brain.llm_preset;
        cfg.jarvis.llm_url = brain.llm_url;
        cfg.jarvis.llm_model = brain.llm_model;
        config::save(&cfg);
    }
    // Plik z kluczami usuwamy, gdy wszystko jest już w sejfie (inaczej spróbujemy przy następnym starcie).
    if !failed {
        let _ = std::fs::remove_file(&path);
    }
    tracing::info!("zaimportowano {imported} kluczy LimboAI z paczki");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_format_skips_missing_keys() {
        let keys = SharedKeys { gemini: Some("g".into()), groq: None, llm: None, brain: None };
        let json = serde_json::to_string(&keys).unwrap();
        assert_eq!(json, r#"{"gemini":"g"}"#);
        let back: SharedKeys = serde_json::from_str(r#"{"groq":"x","nieznane":1}"#).unwrap();
        assert_eq!(back.groq.as_deref(), Some("x"));
    }
}
