//! Ustawienia panelu: otwarte kanały i preferencje wyglądu.
//! Plik: %APPDATA%\LiMBOCHAT\obs-dock.json (osobny od aplikacji PC).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub channels: Vec<String>,
    pub active: Option<String>,
    pub animations: bool,
    pub timestamps: bool,
    pub mod_buttons: bool,
    /// Kolor motywu panelu (#RRGGBB) — z niego liczona jest cała paleta.
    pub accent: String,
    /// Ustawienia Jarvisa (klucze API NIE są tu zapisywane — są w sejfie).
    pub jarvis: crate::jarvis::JarvisConfig,
    /// Overlay LimboAI na stream (źródło przeglądarki /overlay).
    pub overlay: OverlayConfig,
    /// Aktualizacje silnika: `ask` (LimboAI ogłasza i pyta, instalacja na żądanie)
    /// albo `auto` (same, gdy nic się nie dzieje).
    pub updates: String,
    /// Ustawienia przeszły już zmianę domyślnego trybu aktualizacji na `ask`.
    /// Brak pola w pliku = `false` (nie wartość z `Config::default()`).
    #[serde(default)]
    pub updates_v2: bool,
    /// Alerty (bitsy, suby, raidy, follow…) i rymowanki LimboAI.
    pub alerts: AlertsConfig,
    /// Obrazki z linków kappa.lol na czacie: `all`, `trusted` (sub/VIP/mod) albo `off`.
    pub media_previews: String,
    /// Powitania VIP-ów (twórca LiMBOCHAT i własna lista).
    pub greetings: GreetingsConfig,
    /// Sterowanie OBS (obs-websocket).
    pub obs: crate::obs::ObsConfig,
    /// Pamięć LimboAI o widzach.
    pub memory: MemoryConfig,
    /// Podsumowanie streama po zakończeniu.
    pub summary: SummaryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MemoryConfig {
    pub enabled: bool,
    /// LimboAI sam wyciąga fakty z czatu (co 10 minut, bez danych wrażliwych).
    pub learn: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self { enabled: true, learn: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SummaryConfig {
    pub enabled: bool,
    /// LimboAI mówi krótkie podsumowanie na głos.
    pub voice: bool,
}

impl Default for SummaryConfig {
    fn default() -> Self {
        Self { enabled: true, voice: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GreetingsConfig {
    /// Witaj twórcę LiMBOCHAT (kosmiaczek), gdy przywita się na czacie.
    pub creator: bool,
    /// LimboAI mówi powitanie na głos.
    pub voice: bool,
    /// Efekt na overlayu.
    pub overlay: bool,
    /// Własni VIP-owie: (login, tekst powitania).
    pub custom: Vec<(String, String)>,
}

impl Default for GreetingsConfig {
    fn default() -> Self {
        Self { creator: true, voice: true, overlay: true, custom: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AlertsConfig {
    /// Animowane karty alertów na czacie.
    pub enabled: bool,
    /// LimboAI układa rymowankę do nicku osoby z alertu.
    pub rhymes: bool,
    /// Czytaj rymowankę na głos.
    pub rhymes_voice: bool,
    /// Wysyłaj rymowankę na czat Twitcha (jako Ty).
    pub rhymes_chat: bool,
    /// Follow i nagrody za punkty kanału (EventSub, Twój kanał).
    pub follows: bool,
    pub redemptions: bool,
    /// „Pierwszy raz na czacie”.
    pub new_chatters: bool,
    /// Krótki dźwięk w panelu przy alercie.
    pub sound: bool,
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rhymes: true,
            rhymes_voice: false,
            rhymes_chat: false,
            follows: true,
            redemptions: true,
            new_chatters: false,
            sound: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OverlayConfig {
    /// Pokazuj widzom, co powiedziałeś (polecenie).
    pub show_command: bool,
    /// Pokazuj wykonane akcje (np. „Kategoria → GTA 4”).
    pub show_actions: bool,
    /// Pozycja: bottom-left / bottom-right / bottom-center / top-left / top-right / top-center.
    pub position: String,
    /// Skala 70–160 %.
    pub scale: u16,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self { show_command: true, show_actions: true, position: "bottom-left".into(), scale: 100 }
    }
}

impl OverlayConfig {
    pub fn sanitized(mut self) -> Self {
        const POSITIONS: [&str; 6] = ["bottom-left", "bottom-right", "bottom-center", "top-left", "top-right", "top-center"];
        if !POSITIONS.contains(&self.position.as_str()) {
            self.position = "bottom-left".into();
        }
        self.scale = self.scale.clamp(70, 160);
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            active: None,
            animations: true,
            timestamps: true,
            mod_buttons: true,
            accent: DEFAULT_ACCENT.to_owned(),
            jarvis: crate::jarvis::JarvisConfig::default(),
            overlay: OverlayConfig::default(),
            updates: "ask".to_owned(),
            updates_v2: true,
            alerts: AlertsConfig::default(),
            media_previews: "all".to_owned(),
            greetings: GreetingsConfig::default(),
            obs: crate::obs::ObsConfig::default(),
            memory: MemoryConfig::default(),
            summary: SummaryConfig::default(),
        }
    }
}

pub const DEFAULT_ACCENT: &str = "#8b6cf0";

/// `#RRGGBB` → znormalizowany zapis małymi literami.
pub fn parse_accent(s: &str) -> Option<String> {
    let s = s.trim();
    let hex = s.strip_prefix('#')?;
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{}", hex.to_ascii_lowercase()))
}

pub fn data_dir() -> Option<PathBuf> {
    if let Some(path)=std::env::var_os("LIMBOCHAT_DATA_DIR").filter(|path|!path.is_empty()){return Some(PathBuf::from(path));}
    directories::BaseDirs::new().map(|b| b.config_dir().join("LiMBOCHAT"))
}

fn path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("obs-dock.json"))
}

pub fn load() -> Config {
    let mut config: Config = path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    migrate(&mut config);
    config
}

/// Starsze ustawienia miały domyślnie `auto` — raz przełączamy na `ask`
/// (LimboAI pyta o aktualizację). Potem wybór z panelu zostaje.
fn migrate(config: &mut Config) {
    if !config.updates_v2 {
        config.updates = "ask".to_owned();
        config.updates_v2 = true;
    }
}

pub fn save(config: &Config) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string_pretty(config) {
        Ok(json) => {
            // Zapis przez plik tymczasowy — zamknięcie OBS w trakcie zapisu nie uszkodzi ustawień.
            let temp = path.with_extension("json.tmp");
            if std::fs::write(&temp, json).is_ok() {
                let _ = std::fs::rename(temp, path);
            }
        }
        Err(e) => tracing::warn!("nie udało się zapisać ustawień: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_switch_once_to_asking() {
        let mut old: Config = serde_json::from_str(r#"{ "updates": "auto" }"#).unwrap();
        migrate(&mut old);
        assert_eq!((old.updates.as_str(), old.updates_v2), ("ask", true));
        let mut chosen: Config = serde_json::from_str(r#"{ "updates": "auto", "updates_v2": true }"#).unwrap();
        migrate(&mut chosen);
        assert_eq!(chosen.updates, "auto", "wybór streamera z panelu zostaje");
        assert_eq!(Config::default().updates, "ask");
    }
}
