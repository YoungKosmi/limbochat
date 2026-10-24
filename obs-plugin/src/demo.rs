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
