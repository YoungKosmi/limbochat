//! Tryb demo panelu: zmienna środowiskowa `LIMBOCHAT_DEMO=1`.
//!
//! Nie łączy się z Twitchem — generuje wiadomości z kolorowymi emotkami
//! (SVG serwowane przez wtyczkę), żeby obejrzeć panel i animacje bez sieci.

use chrono::Utc;

use crate::{
    emotes::{Emote, EmoteMap, EmoteProvider, EmoteScale},
    events::{AlertKind, ChatAlert},
    parser::{self, Badge, ParsedMessage},
};

/// Obrazek „z kappa.lol” w trybie demo (bez sieci).
pub static KAPPA_DEMO: &[u8] = include_bytes!("../../assets/demo-kappa.png");

pub fn enabled() -> bool {
    std::env::var("LIMBOCHAT_DEMO").is_ok_and(|v| v == "1")
}

const EMOTES: &[(&str, &str, u32, bool)] = &[
    ("KEKW", "#f1c40f", 32, false),
    ("catJAM", "#e67e22", 32, false),
    ("pepeD", "#27ae60", 32, false),
    ("catKISS", "#e74c3c", 32, false),
    ("wideVIBE", "#9b59b6", 96, false),
    ("RainTime", "#3498db", 32, true),
];

pub fn emote_map() -> EmoteMap {
    let emotes = EMOTES
        .iter()
        .map(|(name, _, width, zero_width)| Emote {
            id: (*name).into(),
            name: (*name).into(),
            url: format!("/demo/{name}.svg").into(),
            width: *width as f32,
            height: 32.0,
            animated: false,
            zero_width: *zero_width,
            provider: EmoteProvider::SevenTv,
        })
        .collect();
    EmoteMap::new(emotes, Vec::new())
}

/// Obrazek emotki demo (kółko lub elipsa w kolorze).
pub fn svg(name: &str) -> Option<String> {
    let (_, color, width, zero_width) = EMOTES.iter().find(|(n, ..)| *n == name)?;
    let opacity = if *zero_width { 0.45 } else { 1.0 };
    Some(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="32" viewBox="0 0 {width} 32"><ellipse cx="{cx}" cy="16" rx="{rx}" ry="15" fill="{color}" fill-opacity="{opacity}"/></svg>"#,
        cx = width / 2,
        rx = width / 2 - 1,
    ))
}

const USERS: &[(&str, Option<&str>, &[&str])] = &[
    ("Streamer", Some("#ff4500"), &["broadcaster"]),
    ("ModZiomek", Some("#0000ff"), &["moderator"]),
    ("widz_123", None, &["subscriber"]),
    ("SzerokiFan", Some("#2e8b57"), &["vip"]),
    ("Gaduła", Some("#daa520"), &[]),
    ("NocnyMarek", Some("#ff69b4"), &["subscriber"]),
    ("kasia_gra", None, &[]),
];

const LINES: &[&str] = &[
    "witam wszystkich catJAM catJAM",
    "@ty gramy dziś KEKW",
    "KEKW RainTime deszczowy kekw",
    "wideVIBE szeroka emotka wideVIBE",
    "ale akcja pepeD pepeD pepeD",
    "kto jeszcze ogląda o tej porze catKISS",
    "zobaczcie https://7tv.app KEKW",
    "to była długa wiadomość, która zawinie się w wąskim panelu OBS, żeby było widać, jak emotki catJAM płyną razem z tekstem KEKW",
    "GG",
    "@ty pozdro z Poznania catKISS",
    "patrzcie co znalazłem kappa.lol/demo-kot.png KEKW",
];

pub fn message(channel: &str, n: u64, map: &EmoteMap) -> ParsedMessage {
    let (name, color, badges) = USERS[(n as usize * 5 + 2) % USERS.len()];
    let text = LINES[(n as usize * 7 + 1) % LINES.len()];
    build(channel, &format!("demo-{channel}-{n}"), name, color, badges, text, map)
}

pub fn echo(channel: &str, text: &str, map: &EmoteMap) -> ParsedMessage {
    let id = format!("echo-{}", Utc::now().timestamp_nanos_opt().unwrap_or_default());
    build(channel, &id, "ty", Some("#b9a6f7"), &["moderator"], text, map)
}

fn build(channel: &str, id: &str, name: &str, color: Option<&str>, badges: &[&str], text: &str, map: &EmoteMap) -> ParsedMessage {
    let rgb = color.and_then(|c| {
        let v = u32::from_str_radix(c.trim_start_matches('#'), 16).ok()?;
        Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
    });
    ParsedMessage {
        id: id.to_owned(),
        channel_login: channel.to_owned(),
        channel_id: "0".into(),
        sender_id: format!("uid-{name}"),
        sender_login: name.to_lowercase(),
        sender_name: name.to_owned(),
        color: rgb,
        badges: badges.iter().map(|b| Badge { name: (*b).into(), version: "1".into() }).collect(),
        is_action: false,
        timestamp: Utc::now(),
        segments: parser::tokenize(text, &[], map, EmoteScale::X1),
        raw_text: text.to_owned(),
        deleted: false,
    }
}

/// Przykładowy alert danego rodzaju (demo i przycisk „Pokaż przykład” w ustawieniach).
pub fn alert(channel: &str, kind: &str, map: &EmoteMap) -> ChatAlert {
    let n = Utc::now().timestamp_millis();
    let (name, color, kind, text) = match kind {
        "sub" => ("NocnyMarek", Some("#ff69b4"), AlertKind::Sub { tier: "1".into(), months: 14, resub: true }, Some("rok i dwa miesiące catJAM")),
        "gift" => ("Gaduła", Some("#daa520"), AlertKind::GiftSub { tier: "1".into(), recipient: "kasia_gra".into(), anonymous: false }, None),
        "mass_gift" => ("SzerokiFan", Some("#2e8b57"), AlertKind::MassGift { tier: "1".into(), count: 5, total: Some(25), anonymous: false }, None),
        "raid" => ("Rajderka", Some("#ff8c00"), AlertKind::Raid { viewers: 42, avatar: String::new() }, None),
        "follow" => ("nowy_widz", None, AlertKind::Follow, None),
        "redemption" => (
            "kasia_gra",
            None,
            AlertKind::Redemption { reward: "Nawodnij się".into(), cost: 500, input: Some("pij wodę streamerze!".into()) },
            None,
        ),
        "bits_big" => ("ModZiomek", Some("#0000ff"), AlertKind::Bits { amount: 10_000 }, Some("Cheer10000 za ten clutch pepeD")),
        _ => ("widz_123", Some("#9146ff"), AlertKind::Bits { amount: 500 }, Some("Cheer500 trzymaj na kawę KEKW")),
    };
    let message = text.map(|t| {
        let mut m = build(channel, &format!("alert-msg-{n}"), name, color, &["subscriber"], t, map);
        m.id = format!("alert-{n}");
        m
    });
    ChatAlert {
        id: format!("alert-{n}"),
        channel_login: channel.to_owned(),
        user_id: format!("uid-{name}"),
        user_login: name.to_lowercase(),
        user_name: name.to_owned(),
        color: color.and_then(|c| {
            let v = u32::from_str_radix(c.trim_start_matches('#'), 16).ok()?;
            Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
        }),
        kind,
        message,
        timestamp: Utc::now(),
    }
}

/// Kolejność alertów w trybie demo.
pub const ALERT_CYCLE: [&str; 8] = ["bits", "sub", "follow", "mass_gift", "redemption", "raid", "gift", "bits_big"];

/// Przykładowe statystyki 97-minutowego streama (podgląd podsumowania w trybie demo).
pub fn stream_stats(started: chrono::DateTime<chrono::Utc>) -> crate::summary::Stats {
    use chrono::Duration;
    let mut s = crate::summary::Stats::start("demo", started);
    let people = [("kasia_gra", "kasia_gra"), ("gadula", "Gaduła"), ("modziomek", "ModZiomek"), ("nocnymarek", "NocnyMarek"),
        ("szerokifan", "SzerokiFan"), ("widz_123", "widz_123"), ("pixelka", "Pixelka"), ("ziomal99", "Ziomal99")];
    let emotes = ["KEKW", "catJAM", "pepeD", "KEKW", "catKISS", "PogU", "KEKW", "wideVIBE"];
    let mut n: u64 = 7;
    for minute in 0..97i64 {
        let at = started + Duration::minutes(minute);
        // Widzowie: rozkręcenie, szczyt po raidzie, spadek na koniec.
        let base = 38 + minute * 6 / 5 - if minute > 80 { (minute - 80) * 3 } else { 0 } + if minute > 62 { 35 } else { 0 };
        s.on_viewers((base + (minute * 7 % 9)) as u64, at);
        let per_min = 2 + (minute * 13 % 7) as usize + if (60..72).contains(&minute) { 6 } else { 0 };
        for _ in 0..per_min {
            n = n.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let (login, name) = people[[0, 0, 1, 1, 1, 2, 3, 4, 4, 5, 6, 7][((n >> 33) % 12) as usize]];
            let emote = emotes[((n >> 40) % 8) as usize];
            let with = if (n >> 20) % 3 == 0 { vec![emote.to_owned()] } else { Vec::new() };
            s.on_message(login, name, "gg", &with, at);
        }
    }
    for _ in 0..9 { s.on_new_chatter(); }
    for _ in 0..14 { s.on_follow(); }
    for _ in 0..6 { s.on_redemption(); }
    s.on_bits("szerokifan", "SzerokiFan", 500);
    s.on_bits("pixelka", "Pixelka", 100);
    s.on_sub("nocnymarek", "NocnyMarek", true);
    s.on_sub("ziomal99", "Ziomal99", false);
    s.on_gifts("modziomek", "ModZiomek", 5);
    s.on_raid("ZaprzyjaznionyStreamer", 34);
    s
}
