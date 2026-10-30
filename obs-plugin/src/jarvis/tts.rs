//! Naturalny głos LimboAI: Gemini TTS (Interactions API).
//!
//! Tekst odpowiedzi idzie do modelu mowy Gemini, wraca dźwięk (WAV albo surowe
//! PCM 16-bit, 24 kHz, mono). Głośność nakładamy sami (`Voice::play_speech`).
//! Każdy błąd (brak klucza, limit, brak internetu) = Jarvis mówi głosem systemowym.

use std::time::Duration;

use anyhow::{Context, anyhow};
use base64::Engine;
use serde_json::{Value, json};

use super::{gemini::describe_error, stt::gemini_base};

/// Model mowy (najszybszy z obsługą polskiego). Do testów: `LIMBOCHAT_TTS_MODEL`.
pub const MODEL: &str = "gemini-3.8-flash-lite-tts";
pub const DEFAULT_VOICE: &str = "Kore";
/// Głosy Gemini pokazywane w ustawieniach (nazwa, opis).
pub const VOICES: &[(&str, &str)] = &[
    ("Kore", "Kore — stanowczy"),
    ("Puck", "Puck — wesoły"),
    ("Charon", "Charon — spokojny, rzeczowy"),
    ("Fenrir", "Fenrir — energiczny"),
    ("Aoede", "Aoede — lekki"),
    ("Leda", "Leda — młodzieńczy"),
    ("Zephyr", "Zephyr — jasny"),
    ("Orus", "Orus — zdecydowany"),
];
/// Sposób mówienia (adnotacja `speech_metadata`; tekst jest czytany dosłownie).
const STYLE: &str = "friendly, natural and lively conversational tone of a Polish stream assistant";
/// Dłuższych tekstów nie wysyłamy do mowy (koszt i czas) — reszta i tak jest w panelu.
const MAX_CHARS: usize = 600;
/// Częstotliwość surowego PCM (gdy odpowiedź nie ma nagłówka WAV).
const RAW_RATE: u32 = 24_000;

pub struct Speech {
    pub samples: Vec<f32>,
    pub rate: u32,
}

impl Speech {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f32(self.samples.len() as f32 / self.rate.max(1) as f32)
    }
}

pub fn valid_voice(voice: &str) -> &'static str {
    VOICES.iter().map(|(id, _)| *id).find(|id| id.eq_ignore_ascii_case(voice.trim())).unwrap_or(DEFAULT_VOICE)
}

fn model() -> String {
    std::env::var("LIMBOCHAT_TTS_MODEL").unwrap_or_else(|_| MODEL.to_owned())
}

fn request_body(model: &str, voice: &str, text: &str) -> Value {
    let text: String = text.chars().take(MAX_CHARS).collect();
    json!({
        "model": model,
        "input": [{
            "type": "user_input",
            "content": [{
                "type": "text",
                "text": text,
                "annotations": [{ "type": "speech_metadata", "style": STYLE }],
            }],
        }],
        "response_format": { "type": "audio" },
        "generation_config": { "speech_config": [{ "voice": valid_voice(voice) }] },
    })
}

pub async fn synthesize(http: &reqwest::Client, key: &str, voice: &str, text: &str) -> anyhow::Result<Speech> {
    let resp = http
        .post(format!("{}/interactions", gemini_base()))
        .header("x-goog-api-key", key)
        .json(&request_body(&model(), voice, text))
        .send()
        .await
        .context("Brak połączenia z Gemini (głos)")?;
    let status = resp.status();
    super::usage::record(super::usage::Kind::Voice, status.as_u16());
    let value: Value = resp.json().await.context("Niezrozumiała odpowiedź Gemini (głos)")?;
    if !status.is_success() {
        return Err(anyhow!(describe_error(status.as_u16(), &value)));
    }
    let data = audio_data(&value).ok_or_else(|| anyhow!("Gemini nie zwróciło dźwięku."))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).context("Uszkodzony dźwięk z Gemini")?;
    decode_audio(&bytes).ok_or_else(|| anyhow!("Nieobsługiwany format dźwięku z Gemini."))
}

/// Dźwięk z odpowiedzi: `steps[].content[]` typu `audio` (ostatni), zapasowo stary format `inlineData`.
fn audio_data(value: &Value) -> Option<&str> {
    let from_steps = value["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|step| step["content"].as_array().into_iter().flatten())
        .filter(|part| part["type"] == "audio")
        .filter_map(|part| part["data"].as_str())
        .last();
    from_steps.or_else(|| {
        value["candidates"][0]["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part["inlineData"]["data"].as_str())
            .last()
    })
}

/// WAV (PCM 16-bit) albo surowe PCM 16-bit 24 kHz mono → próbki -1..1.
fn decode_audio(bytes: &[u8]) -> Option<Speech> {
    let (pcm, rate, channels) = if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        parse_wav(bytes)?
    } else {
        (bytes, RAW_RATE, 1)
    };
    let channels = usize::from(channels.max(1));
    let samples: Vec<f32> = pcm
        .chunks_exact(2 * channels)
        .map(|frame| {
            let sum: f32 = frame.chunks_exact(2).map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0).sum();
            sum / channels as f32
        })
        .collect();
    (!samples.is_empty()).then_some(Speech { samples, rate })
}

fn parse_wav(bytes: &[u8]) -> Option<(&[u8], u32, u16)> {
    let mut format: Option<(u32, u16)> = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body_start = at + 8;
        // Strumień może mieć rozmiar „nieznany” (0xFFFFFFFF) — bierzemy to, co jest.
        let body_end = body_start.saturating_add(len).min(bytes.len());
        let body = &bytes[body_start..body_end];
        match id {
            b"fmt " if body.len() >= 16 => {
                let kind = u16::from_le_bytes([body[0], body[1]]);
                let channels = u16::from_le_bytes([body[2], body[3]]);
                let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                let bits = u16::from_le_bytes([body[14], body[15]]);
                if kind != 1 || bits != 16 {
                    return None;
                }
                format = Some((rate, channels));
            }
            b"data" => {
                let (rate, channels) = format?;
                return Some((body, rate, channels));
            }
            _ => {}
        }
        at = body_end + (len & 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_single_speaker_with_known_voice() {
        let body = request_body("m", "puck", "Cześć!");
        assert_eq!(body["model"], "m");
        assert_eq!(body["input"][0]["type"], "user_input");
        assert_eq!(body["input"][0]["content"][0]["text"], "Cześć!");
        assert_eq!(body["response_format"]["type"], "audio");
        assert_eq!(body["generation_config"]["speech_config"][0]["voice"], "Puck");
        let unknown = request_body("m", "<script>", &"a".repeat(2000));
        assert_eq!(unknown["generation_config"]["speech_config"][0]["voice"], DEFAULT_VOICE);
        assert_eq!(unknown["input"][0]["content"][0]["text"].as_str().unwrap().len(), MAX_CHARS);
    }

    #[test]
    fn finds_audio_in_both_response_formats() {
        let steps = json!({ "steps": [
            { "type": "thought", "content": [] },
            { "type": "model_output", "content": [{ "type": "text", "text": "x" }, { "type": "audio", "data": "QUJD" }] }
        ]});
        assert_eq!(audio_data(&steps), Some("QUJD"));
        let old = json!({ "candidates": [{ "content": { "parts": [{ "inlineData": { "mimeType": "audio/L16", "data": "REVG" } }] } }] });
        assert_eq!(audio_data(&old), Some("REVG"));
        assert_eq!(audio_data(&json!({ "steps": [] })), None);
    }

    #[test]
    fn decodes_wav_and_raw_pcm() {
        let wav = super::super::audio::encode_wav(&[0.0, 0.5, -0.5, 0.25], 24_000);
        let speech = decode_audio(&wav).expect("wav");
        assert_eq!(speech.rate, 24_000);
        assert_eq!(speech.samples.len(), 4);
        assert!((speech.samples[1] - 0.5).abs() < 0.01);

        let raw: Vec<u8> = [1000i16, -1000, 0].iter().flat_map(|s| s.to_le_bytes()).collect();
        let speech = decode_audio(&raw).expect("raw");
        assert_eq!(speech.rate, RAW_RATE);
        assert_eq!(speech.samples.len(), 3);
        assert!(speech.duration() < Duration::from_millis(1));

        assert!(decode_audio(&[]).is_none());
    }
}
