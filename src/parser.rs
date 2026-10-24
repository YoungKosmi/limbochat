//! Parser wiadomości: tekst → lista segmentów gotowych do renderowania.
//!
//! Kluczowa obserwacja: na Twitchu nazwy emotek są ZAWSZE oddzielone
//! białymi znakami. Zamiast regexów czy Aho-Corasick wystarczy więc podział
//! na słowa i jedno wyszukanie w `HashMap` na słowo — O(n) względem
//! długości wiadomości, bez backtrackingu.
//!
//! Priorytet rozwiązywania słowa:
//! 1. natywne emotki Twitcha z tagu IRC `emotes` (to, co faktycznie wysłał
//!    użytkownik, łącznie z emotkami z subskrypcji innych kanałów),
//! 2. emotki kanałowe 7TV,
//! 3. emotki globalne 7TV,
//! 4. link / wzmianka / zwykły tekst.
//!
//! Kolejne zwykłe słowa są sklejane w jeden segment `Text` — to drastycznie
//! zmniejsza liczbę widgetów, które UI musi ułożyć w każdej klatce.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use smallvec::SmallVec;

use crate::emotes::{Emote, EmoteMap, EmoteScale};

/// Znak doklejany przez klientów 7TV/Chatterino, by obejść filtr
/// „identycznej wiadomości”. Niewidoczny, ale psuje dopasowanie emotek.
const INVISIBLE_TAG: char = '\u{E0000}';

#[derive(Debug, Clone)]
pub struct EmoteSegment {
    pub base: Arc<Emote>,
    /// Emotki zero-width nałożone na `base` (rysowane w tym samym miejscu).
    pub overlays: SmallVec<[Arc<Emote>; 2]>,
}

#[derive(Debug, Clone)]
pub enum Segment {
    Text(String),
    Emote(EmoteSegment),
    Mention(String),
    Link(String),
}

#[derive(Debug, Clone)]
pub struct Badge {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct ParsedMessage {
    pub id: String,
    pub channel_login: String,
    pub channel_id: String,
    pub sender_id: String,
    pub sender_login: String,
    pub sender_name: String,
    pub color: Option<[u8; 3]>,
    pub badges: SmallVec<[Badge; 4]>,
    pub is_action: bool,
    pub timestamp: DateTime<Utc>,
    pub segments: SmallVec<[Segment; 8]>,
    /// Oryginalny tekst — do kopiowania i do logów moderacji.
    pub raw_text: String,
    /// Ustawiane przez UI po CLEARMSG/CLEARCHAT. Wiadomość zostaje
    /// widoczna (wyszarzona), żeby moderator wiedział, co zostało usunięte.
    pub deleted: bool,
}

/// Natywna emotka Twitcha z tagu `emotes` wiadomości IRC.
#[derive(Debug, Clone, Copy)]
pub struct TwitchEmoteRef<'a> {
    pub id: &'a str,
    pub code: &'a str,
}

pub fn tokenize(
    text: &str,
    twitch_emotes: &[TwitchEmoteRef<'_>],
    emotes: &EmoteMap,
    scale: EmoteScale,
) -> SmallVec<[Segment; 8]> {
    let mut out: SmallVec<[Segment; 8]> = SmallVec::new();
    let mut text_buf = String::new();

    for raw_word in text.split_whitespace() {
        let word = raw_word.trim_matches(INVISIBLE_TAG);
        if word.is_empty() {
            continue;
        }

        if let Some(emote) = resolve_emote(word, twitch_emotes, emotes, scale) {
            flush_text(&mut text_buf, &mut out);

            // Zero-width nakładamy na bezpośrednio poprzedzającą emotkę.
            // Jeśli przed nią jest tekst, rysujemy ją jak zwykłą emotkę.
            if emote.zero_width {
                if let Some(Segment::Emote(prev)) = out.last_mut() {
                    prev.overlays.push(emote);
                    continue;
                }
            }
            out.push(Segment::Emote(EmoteSegment { base: emote, overlays: SmallVec::new() }));
        } else if is_link(word) {
            flush_text(&mut text_buf, &mut out);
            out.push(Segment::Link(word.to_owned()));
        } else if word.len() > 1 && word.starts_with('@') {
            flush_text(&mut text_buf, &mut out);
            out.push(Segment::Mention(word.to_owned()));
        } else {
            if !text_buf.is_empty() {
                text_buf.push(' ');
            }
            text_buf.push_str(word);
        }
    }

    flush_text(&mut text_buf, &mut out);
    out
}

/// Ponowne parsowanie już przetworzonej wiadomości po załadowaniu nowych
/// emotek (np. 7TV odpowiedziało, gdy pierwsze wiadomości już przyszły).
///
/// Przetwarzamy tylko segmenty `Text` — emotki Twitcha, linki i wzmianki są
/// już rozpoznane, więc lista emotek Twitcha może być pusta. Zwraca `true`,
/// jeśli coś się zmieniło (UI musi wtedy przeliczyć wysokość wiersza).
pub fn retokenize(segments: &mut SmallVec<[Segment; 8]>, emotes: &EmoteMap, scale: EmoteScale) -> bool {
    let has_new_emote = segments.iter().any(|s| match s {
        Segment::Text(t) => t.split(' ').any(|w| emotes.get(w).is_some()),
        _ => false,
    });
    if !has_new_emote {
        return false;
    }

    let old = std::mem::take(segments);
    for seg in old {
        match seg {
            Segment::Text(t) => {
                for sub in tokenize(&t, &[], emotes, scale) {
                    push_merged(segments, sub);
                }
            }
            other => push_merged(segments, other),
        }
    }
    true
}

/// Dokłada segment, zachowując regułę nakładek zero-width na granicy
/// dawnych segmentów (np. `KEKW` z Twitcha + nowo poznany `RainTime`).
fn push_merged(out: &mut SmallVec<[Segment; 8]>, seg: Segment) {
    if let Segment::Emote(e) = &seg {
        if e.base.zero_width && e.overlays.is_empty() {
            if let Some(Segment::Emote(prev)) = out.last_mut() {
                prev.overlays.push(e.base.clone());
                return;
            }
        }
    }
    out.push(seg);
}

#[inline]
fn resolve_emote(
    word: &str,
    twitch_emotes: &[TwitchEmoteRef<'_>],
    emotes: &EmoteMap,
    scale: EmoteScale,
) -> Option<Arc<Emote>> {
    // Wiadomość ma zwykle 0–5 emotek Twitcha — wyszukiwanie liniowe jest
    // tu szybsze niż budowanie HashMapy dla każdej wiadomości.
    if let Some(t) = twitch_emotes.iter().find(|t| t.code == word) {
        return Some(Arc::new(Emote::twitch(t.id, t.code, scale)));
    }
    emotes.get(word).cloned()
}

#[inline]
fn is_link(word: &str) -> bool {
    if word.starts_with("https://") || word.starts_with("http://") {
        return true;
    }
    // Jak Twitch: także adresy bez „https://” — „kappa.lol/abc”, „www.strona.pl”.
    let (host, has_path) = match word.split_once('/') {
        Some((h, _)) => (h, true),
        None => (word, false),
    };
    let labels: Vec<&str> = host.split('.').collect();
    let tld = labels.last().copied().unwrap_or_default();
    labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && (2..=6).contains(&tld.len())
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && (has_path || host.starts_with("www."))
}

/// Adres do otwarcia: link bez schematu dostaje „https://”.
pub fn link_url(word: &str) -> String {
    if word.starts_with("https://") || word.starts_with("http://") { word.to_owned() } else { format!("https://{word}") }
}

#[inline]
fn flush_text(buf: &mut String, out: &mut SmallVec<[Segment; 8]>) {
    if !buf.is_empty() {
        out.push(Segment::Text(std::mem::take(buf)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_links() {
        assert!(is_link("kappa.lol/AbCd.png"));
        assert!(is_link("www.twitch.tv"));
        assert!(is_link("https://7tv.app"));
        assert!(!is_link("np."));
        assert!(!is_link("x.png"));
        assert!(!is_link("itd./coś"));
        assert!(!is_link("3.14/2"));
        assert_eq!(link_url("kappa.lol/x"), "https://kappa.lol/x");
    }
    use crate::emotes::EmoteProvider;

    fn seventv(name: &str, zero_width: bool) -> Emote {
        Emote {
            id: name.into(),
            name: name.into(),
            url: format!("https://cdn.7tv.app/emote/{name}/1x.webp").into(),
            width: 32.0,
            height: 32.0,
            animated: false,
            zero_width,
            provider: EmoteProvider::SevenTv,
        }
    }

    fn map() -> EmoteMap {
        EmoteMap::new(
            vec![seventv("KEKW", false), seventv("RainTime", true)],
            vec![seventv("catJAM", false)],
        )
    }

    fn kinds(segs: &[Segment]) -> Vec<String> {
        segs.iter()
            .map(|s| match s {
                Segment::Text(t) => format!("T({t})"),
                Segment::Emote(e) if e.overlays.is_empty() => format!("E({})", e.base.name),
                Segment::Emote(e) => format!("E({}+{})", e.base.name, e.overlays.len()),
                Segment::Mention(m) => format!("M({m})"),
                Segment::Link(l) => format!("L({l})"),
            })
            .collect()
    }

    #[test]
    fn mixes_text_and_emotes_and_merges_words() {
        let segs = tokenize("ale  akcja KEKW serio catJAM", &[], &map(), EmoteScale::X1);
        assert_eq!(kinds(&segs), ["T(ale akcja)", "E(KEKW)", "T(serio)", "E(catJAM)"]);
    }

    #[test]
    fn zero_width_overlays_previous_emote() {
        let segs = tokenize("KEKW RainTime tekst RainTime", &[], &map(), EmoteScale::X1);
        assert_eq!(kinds(&segs), ["E(KEKW+1)", "T(tekst)", "E(RainTime)"]);
    }

    #[test]
    fn twitch_native_emotes_have_priority() {
        let twitch = [TwitchEmoteRef { id: "25", code: "Kappa" }];
        let segs = tokenize("Kappa KEKW", &twitch, &map(), EmoteScale::X1);
        match &segs[0] {
            Segment::Emote(e) => assert!(e.base.url.contains("/emoticons/v2/25/")),
            other => panic!("oczekiwano emotki, jest {other:?}"),
        }
    }

    #[test]
    fn strips_invisible_tag_and_detects_links_mentions() {
        let msg = "@modek zobacz https://7tv.app KEKW \u{E0000}";
        let segs = tokenize(msg, &[], &map(), EmoteScale::X1);
        assert_eq!(kinds(&segs), ["M(@modek)", "T(zobacz)", "L(https://7tv.app)", "E(KEKW)"]);
    }

    #[test]
    fn retokenize_picks_up_late_emotes() {
        let empty = EmoteMap::default();
        let twitch = [TwitchEmoteRef { id: "25", code: "Kappa" }];
        let mut segs = tokenize("Kappa RainTime hej catJAM", &twitch, &empty, EmoteScale::X1);
        assert_eq!(kinds(&segs), ["E(Kappa)", "T(RainTime hej catJAM)"]);

        assert!(retokenize(&mut segs, &map(), EmoteScale::X1));
        assert_eq!(kinds(&segs), ["E(Kappa+1)", "T(hej)", "E(catJAM)"]);
        assert!(!retokenize(&mut segs, &map(), EmoteScale::X1), "drugi raz nic do zmiany");
    }

    #[test]
    fn emote_names_are_case_sensitive() {
        let segs = tokenize("kekw", &[], &map(), EmoteScale::X1);
        assert_eq!(kinds(&segs), ["T(kekw)"]);
    }
}
