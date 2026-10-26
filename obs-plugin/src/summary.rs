//! Podsumowanie streama: statystyki zbierane w trakcie i raport po zakończeniu.
//!
//! Start: OBS zgłasza rozpoczęcie streama (obs-websocket) albo Twitch pokazuje
//! kanał jako „na żywo”. Koniec: „Zakończ stream” w OBS (albo Twitch offline).
//! Raport trafia do panelu (okno z liczbami i wykresami), LimboAI dopisuje
//! krótkie podsumowanie słowami, a plik zostaje w `%APPDATA%\LiMBOCHAT\podsumowania`.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const BUCKET_SECS: i64 = 300;
const SAMPLE_LINES: usize = 240;

/// Statystyki zbierane w trakcie streama.
pub struct Stats {
    pub id: u64,
    pub channel: String,
    pub started_at: DateTime<Utc>,
    pub title: Option<String>,
    pub category: Option<String>,
    messages: u64,
    chatters: HashMap<String, (String, u64)>,
    buckets: Vec<u32>,
    emotes: HashMap<String, u64>,
    bits: u64,
    supporters: HashMap<String, (String, u64)>,
    subs: u32,
    resubs: u32,
    gifts: u64,
    raids: Vec<(String, u64)>,
    follows: u32,
    redemptions: u32,
    new_chatters: u32,
    viewers: Vec<(i64, u64)>,
    clips: Vec<String>,
    sample: VecDeque<(String, String)>,
}

impl Stats {
    pub fn start(channel: &str, started_at: DateTime<Utc>) -> Self {
        Self {
            id: started_at.timestamp_millis() as u64,
            channel: channel.to_owned(),
            started_at,
            title: None,
            category: None,
            messages: 0,
            chatters: HashMap::new(),
            buckets: Vec::new(),
            emotes: HashMap::new(),
            bits: 0,
            supporters: HashMap::new(),
            subs: 0,
            resubs: 0,
            gifts: 0,
            raids: Vec::new(),
            follows: 0,
            redemptions: 0,
            new_chatters: 0,
            viewers: Vec::new(),
            clips: Vec::new(),
            sample: VecDeque::new(),
        }
    }

    fn bucket(&mut self, at: DateTime<Utc>) -> &mut u32 {
        let i = ((at - self.started_at).num_seconds().max(0) / BUCKET_SECS) as usize;
        if self.buckets.len() <= i {
            self.buckets.resize(i + 1, 0);
        }
        &mut self.buckets[i]
    }

    pub fn on_message(&mut self, login: &str, name: &str, text: &str, emotes: &[String], at: DateTime<Utc>) {
        self.messages += 1;
        *self.bucket(at) += 1;
        self.chatters.entry(login.to_lowercase()).or_insert_with(|| (name.to_owned(), 0)).1 += 1;
        for e in emotes {
            *self.emotes.entry(e.clone()).or_default() += 1;
        }
        self.sample.push_back((name.to_owned(), text.chars().take(200).collect()));
        if self.sample.len() > SAMPLE_LINES {
            self.sample.pop_front();
        }
    }

    pub fn on_bits(&mut self, login: &str, name: &str, amount: u64) {
        self.bits += amount;
        self.support(login, name, amount);
    }

    fn support(&mut self, login: &str, name: &str, value: u64) {
        self.supporters.entry(login.to_lowercase()).or_insert_with(|| (name.to_owned(), 0)).1 += value;
    }

    pub fn on_sub(&mut self, login: &str, name: &str, resub: bool) {
        if resub {
            self.resubs += 1;
        } else {
            self.subs += 1;
        }
        // Sub ≈ 500 bitsów — żeby ranking wsparcia był porównywalny.
        self.support(login, name, 500);
    }

    pub fn on_gifts(&mut self, login: &str, name: &str, count: u64) {
        self.gifts += count;
        if !login.is_empty() {
            self.support(login, name, 500 * count);
        }
    }

    pub fn on_raid(&mut self, name: &str, viewers: u64) {
        self.raids.push((name.to_owned(), viewers));
    }

    pub fn on_follow(&mut self) {
        self.follows += 1;
    }

    pub fn on_redemption(&mut self) {
        self.redemptions += 1;
    }

    pub fn on_new_chatter(&mut self) {
        self.new_chatters += 1;
    }

    pub fn on_viewers(&mut self, count: u64, at: DateTime<Utc>) {
        self.viewers.push(((at - self.started_at).num_minutes(), count));
    }

    pub fn on_clip(&mut self, url: &str) {
        self.clips.push(url.to_owned());
    }

    /// Kopia do podsumowania „w trakcie” (statystyki zbierają się dalej).
    pub fn snapshot(&self) -> Stats {
        Stats {
            id: self.id,
            channel: self.channel.clone(),
            started_at: self.started_at,
            title: self.title.clone(),
            category: self.category.clone(),
            messages: self.messages,
            chatters: self.chatters.clone(),
            buckets: self.buckets.clone(),
            emotes: self.emotes.clone(),
            bits: self.bits,
            supporters: self.supporters.clone(),
            subs: self.subs,
            resubs: self.resubs,
            gifts: self.gifts,
            raids: self.raids.clone(),
            follows: self.follows,
            redemptions: self.redemptions,
            new_chatters: self.new_chatters,
            viewers: self.viewers.clone(),
            clips: self.clips.clone(),
            sample: self.sample.clone(),
        }
    }

    /// Ostatnie linie czatu (dla podsumowania słowami).
    pub fn sample(&self) -> Vec<(String, String)> {
        self.sample.iter().cloned().collect()
    }

    pub fn finish(&self, ended_at: DateTime<Utc>) -> Summary {
        let mut top: Vec<(String, u64)> = self.chatters.values().cloned().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut emotes: Vec<(String, u64)> = self.emotes.iter().map(|(k, v)| (k.clone(), *v)).collect();
        emotes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut supporters: Vec<(String, u64)> = self.supporters.values().cloned().collect();
        supporters.sort_by(|a, b| b.1.cmp(&a.1));
        let peak = self.viewers.iter().map(|v| v.1).max().unwrap_or(0);
        let avg = if self.viewers.is_empty() { 0 } else { self.viewers.iter().map(|v| v.1).sum::<u64>() / self.viewers.len() as u64 };
        let minutes = (ended_at - self.started_at).num_minutes().max(0) as u64;
        let mut buckets = self.buckets.clone();
        buckets.resize(((minutes as i64 * 60) / BUCKET_SECS + 1) as usize, 0);
        Summary {
            id: self.id,
            channel: self.channel.clone(),
            started_at: self.started_at.timestamp_millis(),
            ended_at: ended_at.timestamp_millis(),
            minutes,
            title: self.title.clone(),
            category: self.category.clone(),
            messages: self.messages,
            chatters: self.chatters.len() as u64,
            top_chatters: top.into_iter().take(5).collect(),
            top_emotes: emotes.into_iter().take(5).collect(),
            top_supporters: supporters.into_iter().take(3).collect(),
            peak_viewers: peak,
            avg_viewers: avg,
            viewers: self.viewers.clone(),
            messages_per_5min: buckets,
            bits: self.bits,
            subs: self.subs,
            resubs: self.resubs,
            gifts: self.gifts,
            raids: self.raids.clone(),
            follows: self.follows,
            redemptions: self.redemptions,
            new_chatters: self.new_chatters,
            clips: self.clips.clone(),
            ai_text: None,
            ai_pending: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub id: u64,
    pub channel: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub minutes: u64,
    pub title: Option<String>,
    pub category: Option<String>,
    pub messages: u64,
    pub chatters: u64,
    pub top_chatters: Vec<(String, u64)>,
    pub top_emotes: Vec<(String, u64)>,
    pub top_supporters: Vec<(String, u64)>,
    pub peak_viewers: u64,
    pub avg_viewers: u64,
    /// (minuta streama, widzowie)
    pub viewers: Vec<(i64, u64)>,
    pub messages_per_5min: Vec<u32>,
    pub bits: u64,
    pub subs: u32,
    pub resubs: u32,
    pub gifts: u64,
    pub raids: Vec<(String, u64)>,
    pub follows: u32,
    pub redemptions: u32,
    pub new_chatters: u32,
    pub clips: Vec<String>,
    /// Podsumowanie słowami od LimboAI (przychodzi chwilę później).
    pub ai_text: Option<String>,
    pub ai_pending: bool,
}

impl Summary {
    /// Krótko na głos: „Stream zakończony! 2 godziny 14 minut, …”.
    pub fn spoken(&self) -> String {
        let (h, m) = (self.minutes / 60, self.minutes % 60);
        let time = match (h, m) {
            (0, m) => format!("{m} minut"),
            (h, 0) => format!("{h} {}", hours(h)),
            (h, m) => format!("{h} {} i {m} minut", hours(h)),
        };
        let mut s = format!("Stream zakończony! {time}, {} wiadomości na czacie", self.messages);
        if self.peak_viewers > 0 {
            s += &format!(", rekord {} widzów", self.peak_viewers);
        }
        s + ". Podsumowanie czeka w panelu."
    }

    /// Dane dla LimboAI do podsumowania słowami.
    pub fn prompt(&self, sample: &[(String, String)]) -> String {
        let mut p = format!(
            "Stream: {} min, tytuł: {}, kategoria: {}. Wiadomości: {}, osób na czacie: {}, nowych: {}. \
Widzowie: max {}, średnio {}. Bitsy: {}, suby: {}, resuby: {}, gifty: {}, follow: {}, nagrody: {}. ",
            self.minutes,
            self.title.as_deref().unwrap_or("?"),
            self.category.as_deref().unwrap_or("?"),
            self.messages,
            self.chatters,
            self.new_chatters,
            self.peak_viewers,
            self.avg_viewers,
            self.bits,
            self.subs,
            self.resubs,
            self.gifts,
            self.follows,
            self.redemptions
        );
        if !self.raids.is_empty() {
            p += &format!("Raidy: {}. ", self.raids.iter().map(|(n, v)| format!("{n} ({v})")).collect::<Vec<_>>().join(", "));
        }
        p += &format!("Najaktywniejsi: {}. ", self.top_chatters.iter().map(|(n, c)| format!("{n} ({c})")).collect::<Vec<_>>().join(", "));
        if !self.top_supporters.is_empty() {
            p += &format!("Największe wsparcie: {}. ", self.top_supporters.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "));
        }
        p += "\nFragment czatu (od najstarszych):\n";
        for (name, text) in sample.iter().rev().take(160).rev() {
            p += &format!("{name}: {text}\n");
        }
        p
    }
}

fn hours(h: u64) -> &'static str {
    match h {
        1 => "godzina",
        2..=4 => "godziny",
        _ => "godzin",
    }
}

pub const SUMMARY_SYSTEM: &str = "Jesteś LimboAI, asystent streamera. Na podstawie statystyk i fragmentu czatu \
napisz krótkie podsumowanie zakończonego streama po polsku: 4–6 zdań, ciepło i z humorem. \
Wspomnij, o czym najwięcej gadał czat, najlepsze momenty, podziękuj z nicku najaktywniejszym i wspierającym. \
Nie wymyślaj liczb ani zdarzeń, których nie ma w danych. Bez emoji, nagłówków i list.";

/// Zapis i historia podsumowań (ostatnie 30).
pub fn save(summary: &Summary) {
    let Some(dir) = dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let name = DateTime::from_timestamp_millis(summary.started_at).map_or_else(|| summary.id.to_string(), |d| d.format("%Y-%m-%d_%H-%M").to_string());
    if let Ok(json) = serde_json::to_vec_pretty(summary) {
        let _ = std::fs::write(dir.join(format!("{name}.json")), json);
    }
    // Sprzątanie: zostawiamy 30 najnowszych.
    let mut files: Vec<_> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    while files.len() > 30 {
        let _ = std::fs::remove_file(files.remove(0));
    }
}

pub fn history() -> Vec<Summary> {
    let Some(dir) = dir() else { return Vec::new() };
    let mut files: Vec<_> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    files.iter().rev().take(10).filter_map(|p| serde_json::from_slice(&std::fs::read(p).ok()?).ok()).collect()
}

fn dir() -> Option<std::path::PathBuf> {
    crate::config::data_dir().map(|d| d.join("podsumowania"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_and_summarizes() {
        let t0 = Utc::now() - chrono::Duration::minutes(134);
        let mut s = Stats::start("kanal", t0);
        for i in 0..30 {
            s.on_message("kasia", "kasia", "hej KEKW", &["KEKW".into()], t0 + chrono::Duration::minutes(i));
        }
        s.on_message("marek", "Marek", "gg", &[], t0 + chrono::Duration::minutes(100));
        s.on_bits("marek", "Marek", 1000);
        s.on_sub("kasia", "kasia", true);
        s.on_gifts("", "Anonim", 5);
        s.on_viewers(40, t0 + chrono::Duration::minutes(10));
        s.on_viewers(87, t0 + chrono::Duration::minutes(60));
        let sum = s.finish(t0 + chrono::Duration::minutes(134));
        assert_eq!((sum.minutes, sum.messages, sum.chatters, sum.peak_viewers, sum.avg_viewers), (134, 31, 2, 87, 63));
        assert_eq!(sum.top_chatters[0], ("kasia".to_owned(), 30));
        assert_eq!(sum.top_emotes[0], ("KEKW".to_owned(), 30));
        assert_eq!(sum.top_supporters[0].0, "Marek");
        assert_eq!((sum.bits, sum.resubs, sum.gifts), (1000, 1, 5));
        assert_eq!(sum.messages_per_5min.len(), 27);
        assert_eq!(sum.spoken(), "Stream zakończony! 2 godziny i 14 minut, 31 wiadomości na czacie, rekord 87 widzów. Podsumowanie czeka w panelu.");
        assert!(sum.prompt(&s.sample()).contains("kasia: hej KEKW"));
    }
}
