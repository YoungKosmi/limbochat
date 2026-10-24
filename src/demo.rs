//! Tryb demo: `TMC_DEMO=1 cargo run`
//!
//! Wstrzykuje przykładowe wiadomości z emotkami generowanymi lokalnie
//! (`bytes://`), żeby testować układ UI bez sieci i bez Twitcha.

use std::io::Cursor;

use chrono::Utc;
use crate::{
    emotes::{Emote, EmoteMap, EmoteProvider, EmoteScale},
    parser::{self, Badge, ParsedMessage},
    ui::chat_view::{ChatView, SystemLevel},
};

pub fn enabled() -> bool {
    std::env::var("TMC_DEMO").is_ok_and(|v| v == "1")
}

/// Generuje prosty obrazek PNG (kółko w kolorze) i rejestruje go w egui.
fn fake_emote(ctx: &egui::Context, name: &str, rgb: [u8; 3], w: u32, h: u32, zero_width: bool) -> Emote {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let (dx, dy) = ((x as f32 - cx) / cx, (y as f32 - cy) / cy);
        let inside = dx * dx + dy * dy <= 1.0;
        let alpha = if zero_width { 110 } else { 255 };
        if inside { image::Rgba([rgb[0], rgb[1], rgb[2], alpha]) } else { image::Rgba([0, 0, 0, 0]) }
    });
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).expect("kodowanie PNG");

    let uri = format!("bytes://demo/{name}.png");
    ctx.include_bytes(uri.clone(), png);

    Emote {
        id: name.into(),
        name: name.into(),
        url: uri.into(),
        width: w as f32,
        height: h as f32,
        animated: false,
        zero_width,
        provider: EmoteProvider::SevenTv,
    }
}

/// Emotki demo (do pickera i podpowiedzi) — rejestruje obrazki w egui.
pub fn emotes(ctx: &egui::Context) -> EmoteMap {
    EmoteMap::new(
        vec![
            fake_emote(ctx, "KEKW", [0xf1, 0xc4, 0x0f], 32, 32, false),
            fake_emote(ctx, "RainTime", [0x34, 0x98, 0xdb], 32, 32, true),
        ],
        vec![
            fake_emote(ctx, "catJAM", [0xe6, 0x7e, 0x22], 32, 32, false),
            fake_emote(ctx, "wideVIBE", [0x9b, 0x59, 0xb6], 96, 32, false),
            fake_emote(ctx, "catKISS", [0xe7, 0x4c, 0x3c], 32, 32, false),
            fake_emote(ctx, "pepeD", [0x27, 0xae, 0x60], 32, 32, false),
        ],
    )
}

fn message(id: &str, name: &str, color: Option<[u8; 3]>, badges: &[&str], is_action: bool, text: &str, map: &EmoteMap) -> ParsedMessage {
    ParsedMessage {
        id: format!("demo-{id}"),
        channel_login: "demo".into(),
        channel_id: "0".into(),
        sender_id: id.to_string(),
        sender_login: name.to_lowercase(),
        sender_name: name.to_string(),
        color,
        badges: badges.iter().map(|b| Badge { name: b.to_string(), version: "1".into() }).collect(),
        is_action,
        timestamp: Utc::now(),
        segments: parser::tokenize(text, &[], map, EmoteScale::X1),
        raw_text: text.to_string(),
        deleted: false,
    }
}

/// „Echo” wysłanej wiadomości w trybie demo (zamiast prawdziwego Helix).
pub fn echo(text: &str, map: &EmoteMap) -> ParsedMessage {
    let id = format!("echo-{}", Utc::now().timestamp_nanos_opt().unwrap_or_default());
    message(&id, "Ty", Some([0x00, 0xbf, 0xff]), &["moderator"], false, text, map)
}

pub fn inject(chat: &mut ChatView, map: &EmoteMap) {
    chat.push_system("Tryb demo — wiadomości i emotki są generowane lokalnie", SystemLevel::Info);

    let samples: &[(&str, &str, Option<[u8; 3]>, &[&str], bool, &str)] = &[
        ("1", "Streamer", Some([0xff, 0x45, 0x00]), &["broadcaster"], false, "witam wszystkich catJAM catJAM"),
        ("2", "ModZiomek", Some([0x00, 0x00, 0xff]), &["moderator"], false, "@Streamer gramy dziś KEKW"),
        ("3", "widz_123", None, &["subscriber"], false, "KEKW RainTime deszczowy kekw (nakładka zero-width)"),
        ("4", "SzerokiFan", Some([0x2e, 0x8b, 0x57]), &["vip"], false, "wideVIBE szeroka emotka zachowuje proporcje wideVIBE"),
        ("5", "Gaduła", Some([0xda, 0xa5, 0x20]), &[], false,
         "bardzo długa wiadomość, która musi się zawinąć do kilku linii, żeby sprawdzić, czy emotki catJAM \
          poprawnie płyną razem z tekstem KEKW i nie rozjeżdżają układu nawet przy wąskim oknie catJAM koniec"),
        ("6", "Akcjonariusz", Some([0xff, 0x69, 0xb4]), &[], true, "tańczy catJAM"),
        ("7", "Linkowiec", None, &[], false, "zobaczcie https://7tv.app KEKW"),
        ("8", "Spamer", Some([0x80, 0x80, 0x80]), &[], false, "KUP TANIE FOLLOWY tutaj"),
    ];

    for (id, name, color, badges, is_action, text) in samples {
        chat.push_message(Box::new(message(id, name, *color, badges, *is_action, text, map)));
    }

    // Pokaż wygląd usuniętej wiadomości i komunikatu o timeoucie.
    chat.mark_message_deleted("demo-8");
    chat.push_system("spamer otrzymał timeout na 10 min", SystemLevel::Info);
}
