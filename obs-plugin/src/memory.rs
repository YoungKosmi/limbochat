//! Pamięć LimboAI o widzach.
//!
//! * Liczniki zbierane same: od kiedy pisze, ile wiadomości, na ilu streamach,
//!   bitsy, miesiące suba, podarowane suby.
//! * Notatki: „lubi koty”, „ma psa Burka”, „gra w CS-a” — dodawane głosem
//!   („LimboAI, zapamiętaj, że…”), w panelu (karta widza) albo automatycznie
//!   przez LimboAI z rozmów na czacie (opcja, bez danych wrażliwych).
//!
//! Pamięć służy rymowankom, odpowiedziom LimboAI i karcie widza w panelu.
//! Plik: `%APPDATA%\LiMBOCHAT\pamiec-widzow.json` — tylko na tym komputerze.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};

const MAX_NOTES: usize = 12;
const MAX_NOTE_LEN: usize = 140;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Viewer {
    pub name: String,
    /// Pierwsza i ostatnia wiadomość (ms od 1970).
    pub first_seen: i64,
    pub last_seen: i64,
    pub messages: u64,
    /// Na ilu streamach pisał(a).
    pub streams: u32,
    /// Ostatni stream, na którym liczyliśmy obecność.
    pub last_stream: u64,
    pub bits: u64,
    pub sub_months: u64,
    pub gifted: u64,
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub text: String,
    pub at: i64,
    /// `ai` (wyciągnięte z czatu), `voice` (polecenie głosowe), `panel` (dopisane ręcznie).
    pub source: String,
}

pub struct Memory {
    path: Option<PathBuf>,
    viewers: Mutex<HashMap<String, Viewer>>,
    dirty: AtomicBool,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn key(login: &str) -> String {
    login.trim().trim_start_matches('@').to_lowercase()
}

impl Memory {
    pub fn load() -> Self {
        let path = crate::config::data_dir().map(|d| d.join("pamiec-widzow.json"));
        let viewers = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, viewers: Mutex::new(viewers), dirty: AtomicBool::new(false) }
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self { path: None, viewers: Mutex::new(HashMap::new()), dirty: AtomicBool::new(false) }
    }

    fn with<R>(&self, f: impl FnOnce(&mut HashMap<String, Viewer>) -> R) -> R {
        let mut map = self.viewers.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut map)
    }

    /// Zapis na dysk, jeśli coś się zmieniło (wołane co minutę i przy zamknięciu).
    pub fn save_if_dirty(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let Some(path) = &self.path else { return };
        let json = self.with(|m| serde_json::to_vec(m).unwrap_or_default());
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }

    fn touch(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Każda wiadomość widza (liczniki). `stream_id` = numer bieżącego streama (0 = brak).
    pub fn on_message(&self, login: &str, name: &str, stream_id: u64) {
        let t = now();
        self.with(|m| {
            let v = m.entry(key(login)).or_insert_with(|| Viewer { first_seen: t, ..Default::default() });
            v.name = name.to_owned();
            v.last_seen = t;
            v.messages += 1;
            if stream_id != 0 && v.last_stream != stream_id {
                v.last_stream = stream_id;
                v.streams += 1;
            }
        });
        self.touch();
    }

    pub fn on_support(&self, login: &str, name: &str, bits: u64, sub_months: Option<u64>, gifted: u64) {
        let t = now();
        self.with(|m| {
            let v = m.entry(key(login)).or_insert_with(|| Viewer { first_seen: t, ..Default::default() });
            if !name.is_empty() {
                v.name = name.to_owned();
            }
            v.bits += bits;
            if let Some(months) = sub_months {
                v.sub_months = v.sub_months.max(months);
            }
            v.gifted += gifted;
        });
        self.touch();
    }

    /// Dopisuje fakt. Zwraca `false`, gdy był już zapisany (albo pusty).
    pub fn remember(&self, login: &str, name: &str, text: &str, source: &str) -> bool {
        let text: String = text.trim().trim_end_matches('.').chars().take(MAX_NOTE_LEN).collect();
        if text.is_empty() || key(login).is_empty() {
            return false;
        }
        let t = now();
        let added = self.with(|m| {
            let v = m.entry(key(login)).or_insert_with(|| Viewer { first_seen: t, name: name.to_owned(), ..Default::default() });
            if v.name.is_empty() {
                v.name = name.to_owned();
            }
            let folded = fold(&text);
            if v.notes.iter().any(|n| fold(&n.text) == folded || fold(&n.text).contains(&folded) || folded.contains(&fold(&n.text))) {
                return false;
            }
            v.notes.push(Note { text: text.clone(), at: t, source: source.to_owned() });
            // Najstarsze notatki z czatu ustępują miejsca nowym (ręczne zostają).
            while v.notes.len() > MAX_NOTES {
                let i = v.notes.iter().position(|n| n.source == "ai").unwrap_or(0);
                v.notes.remove(i);
            }
            true
        });
        if added {
            self.touch();
        }
        added
    }

    /// Usuwa notatkę (po numerze albo pasującą do tekstu); `None` = wszystkie notatki tej osoby.
    pub fn forget(&self, login: &str, which: Forget) -> usize {
        let removed = self.with(|m| {
            let Some(v) = m.get_mut(&key(login)) else { return 0 };
            let before = v.notes.len();
            match which {
                Forget::All => v.notes.clear(),
                Forget::Index(i) => {
                    if i < v.notes.len() {
                        v.notes.remove(i);
                    }
                }
                Forget::Matching(text) => {
                    let wanted = fold(&text);
                    v.notes.retain(|n| !fold(&n.text).contains(&wanted));
                }
            }
            before - v.notes.len()
        });
        if removed > 0 {
            self.touch();
        }
        removed
    }

    pub fn clear(&self) {
        self.with(|m| m.clear());
        self.touch();
    }

    pub fn get(&self, login: &str) -> Option<Viewer> {
        self.with(|m| m.get(&key(login)).cloned())
    }

    pub fn len(&self) -> usize {
        self.with(|m| m.len())
    }

    /// Szuka po nicku z mowy (bez polskich znaków i wielkości liter, dopuszcza drobne literówki).
    pub fn find(&self, spoken: &str) -> Option<(String, Viewer)> {
        let wanted: String = fold(spoken).chars().filter(|c| c.is_alphanumeric()).collect();
        if wanted.is_empty() {
            return None;
        }
        self.with(|m| {
            let norm = |s: &str| fold(s).chars().filter(|c| c.is_alphanumeric()).collect::<String>();
            m.iter()
                .find(|(login, v)| norm(login) == wanted || norm(&v.name) == wanted)
                .or_else(|| m.iter().find(|(login, _)| wanted.len() >= 4 && norm(login).starts_with(&wanted)))
                .map(|(l, v)| (l.clone(), v.clone()))
        })
    }

    /// Krótki opis do promptu: „lubi koty; ma psa Burka; 12 streamów, od 03.2026”.
    pub fn brief(&self, login: &str) -> Option<String> {
        let v = self.get(login)?;
        let mut parts: Vec<String> = v.notes.iter().map(|n| n.text.clone()).collect();
        if v.streams >= 2 {
            parts.push(format!("był(a) na {} streamach", v.streams));
        }
        if v.first_seen > 0 {
            if let Some(d) = chrono::DateTime::from_timestamp_millis(v.first_seen) {
                parts.push(format!("pisze od {}", d.format("%m.%Y")));
            }
        }
        if v.bits > 0 {
            parts.push(format!("dał(a) łącznie {} bitsów", v.bits));
        }
        if v.sub_months > 0 {
            parts.push(format!("sub od {} mies.", v.sub_months));
        }
        if v.gifted > 0 {
            parts.push(format!("podarował(a) {} subów", v.gifted));
        }
        (!parts.is_empty() && !v.notes.is_empty() || parts.len() >= 2).then(|| parts.join("; "))
    }
}

pub enum Forget {
    All,
    Index(usize),
    Matching(String),
}

fn fold(s: &str) -> String {
    s.to_lowercase()
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
        .collect()
}

/// Odpowiedź modelu z faktami: `[{"login": "...", "fact": "..."}]` (także w bloku ```json).
pub fn parse_facts(text: &str) -> Vec<(String, String)> {
    let start = text.find('[');
    let end = text.rfind(']');
    let (Some(start), Some(end)) = (start, end) else { return Vec::new() };
    if end <= start {
        return Vec::new();
    }
    let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&text[start..=end]) else { return Vec::new() };
    list.iter()
        .filter_map(|f| {
            let login = f["login"].as_str()?.trim().trim_start_matches('@').to_lowercase();
            let fact = f["fact"].as_str()?.trim().to_owned();
            (!login.is_empty() && !fact.is_empty() && fact.chars().count() <= MAX_NOTE_LEN).then_some((login, fact))
        })
        .take(20)
        .collect()
}

pub const LEARN_SYSTEM: &str = "Jesteś LimboAI, asystent streamera. Z fragmentu czatu Twitcha wyciągasz TRWAŁE, \
neutralne fakty o widzach, które pomogą streamerowi ich pamiętać: zainteresowania, ulubione gry, zwierzęta, \
hobby, co robią na co dzień ogólnie (np. „studiuje informatykę”), z jakiego regionu są ogólnie.
NIE zapisuj: zdrowia, polityki, religii, orientacji, danych kontaktowych, adresów, nazwisk, numerów, wieku poniżej 18 lat,
plotek o innych osobach, chwilowych stanów („jest głodny”) ani niczego obraźliwego.
Każdy fakt krótko, po polsku, w 3. osobie bez nicku (np. „ma psa Burka”, „gra w CS-a”).
Odpowiedz WYŁĄCZNIE tablicą JSON: [{\"login\": \"nick\", \"fact\": \"...\"}]. Jeśli nic trwałego — [].";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_dedupes_and_forgets() {
        let m = Memory::in_memory();
        assert!(m.remember("Kasia_Gra", "kasia_gra", "lubi koty.", "voice"));
        assert!(!m.remember("kasia_gra", "kasia_gra", "Lubi koty", "ai"), "duplikat");
        assert!(m.remember("kasia_gra", "kasia_gra", "ma psa Burka", "ai"));
        m.on_message("kasia_gra", "kasia_gra", 7);
        m.on_message("kasia_gra", "kasia_gra", 7);
        m.on_message("kasia_gra", "kasia_gra", 8);
        let v = m.get("@KASIA_GRA").unwrap();
        assert_eq!((v.messages, v.streams, v.notes.len()), (3, 2, 2));
        assert!(m.brief("kasia_gra").unwrap().contains("lubi koty; ma psa Burka"));
        assert_eq!(m.find("kasia gra").map(|(l, _)| l).as_deref(), Some("kasia_gra"));
        assert_eq!(m.forget("kasia_gra", Forget::Matching("psa".into())), 1);
        assert_eq!(m.forget("kasia_gra", Forget::All), 1);
    }

    #[test]
    fn note_limit_keeps_manual_notes() {
        let m = Memory::in_memory();
        m.remember("x", "x", "notatka ręczna", "panel");
        for i in 0..20 {
            m.remember("x", "x", &format!("fakt numer {i} z czatu"), "ai");
        }
        let v = m.get("x").unwrap();
        assert_eq!(v.notes.len(), MAX_NOTES);
        assert!(v.notes.iter().any(|n| n.source == "panel"));
    }

    #[test]
    fn parses_model_output() {
        let out = "```json\n[{\"login\": \"@Kasia_Gra\", \"fact\": \"ma kota Mruczka\"}, {\"login\": \"\", \"fact\": \"x\"}]\n```";
        assert_eq!(parse_facts(out), vec![("kasia_gra".to_owned(), "ma kota Mruczka".to_owned())]);
        assert!(parse_facts("Nic nie znalazłem").is_empty());
        assert!(parse_facts("[]").is_empty());
    }
}
