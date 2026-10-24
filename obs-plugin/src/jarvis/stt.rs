//! Rozpoznawanie mowy: Groq Whisper (główne) albo Gemini (zapasowo, gdy
//! nie ma klucza Groq).
//!
//! Adresy API można podmienić zmiennymi `LIMBOCHAT_GROQ_URL` /
//! `LIMBOCHAT_GEMINI_URL` — używane w testach z atrapami serwerów.

use anyhow::{Context, anyhow, bail};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;

const GROQ_MODEL: &str = "whisper-large-v3-turbo";
/// Whisper przyjmuje do ~224 tokenów podpowiedzi (słownictwo, nicki).
const MAX_PROMPT_CHARS: usize = 600;

pub fn groq_base() -> String {
    std::env::var("LIMBOCHAT_GROQ_URL").unwrap_or_else(|_| "https://api.groq.com/openai/v1".to_owned())
}

pub fn gemini_base() -> String {
    std::env::var("LIMBOCHAT_GEMINI_URL").unwrap_or_else(|_| "https://generativelanguage.googleapis.com/v1beta".to_owned())
}

#[derive(Deserialize)]
struct GroqResponse {
    text: String,
}

#[derive(Deserialize, Default)]
struct ApiError {
    #[serde(default)]
    error: ApiErrorBody,
}

#[derive(Deserialize, Default)]
struct ApiErrorBody {
    #[serde(default)]
    message: String,
}

/// Podpowiedź dla Whispera: słownictwo komend i nicki z czatu poprawiają
/// pisownię (np. „ModZiomek” zamiast „mod ziomek”).
pub fn build_prompt(chatters: &[String]) -> String {
    let mut prompt = String::from(
        "Limbo, zmień kategorię na Grand Theft Auto IV. Zmień tytuł. Timeout, ban, raid, klip, marker, ankieta, slow mode, tryb emotek. Twitch, OBS. ",
    );
    for name in chatters {
        if prompt.len() + name.len() + 2 > MAX_PROMPT_CHARS {
            break;
        }
        prompt.push_str(name);
        prompt.push_str(", ");
    }
    prompt
}

pub async fn transcribe_groq(http: &reqwest::Client, key: &str, wav: Vec<u8>, prompt: &str) -> anyhow::Result<String> {
    let part = reqwest::multipart::Part::bytes(wav).file_name("komenda.wav").mime_str("audio/wav")?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", GROQ_MODEL)
        .text("language", "pl")
        .text("response_format", "json")
        .text("temperature", "0")
        .text("prompt", prompt.to_owned());

    let resp = http
        .post(format!("{}/audio/transcriptions", groq_base()))
        .bearer_auth(key)
        .multipart(form)
        .send()
        .await
        .context("Brak połączenia z Groq")?;

    let status = resp.status();
    if !status.is_success() {
        let message = resp.json::<ApiError>().await.unwrap_or_default().error.message;
        bail!(match status.as_u16() {
            401 | 403 => "Klucz Groq jest nieprawidłowy — sprawdź go w ustawieniach LimboAI.".to_owned(),
            429 => "Przekroczono limit Groq — spróbuj za chwilę.".to_owned(),
            _ => format!("Groq: {message} (HTTP {status})"),
        });
    }
    let body: GroqResponse = resp.json().await.context("Niezrozumiała odpowiedź Groq")?;
    Ok(clean(&body.text))
}

/// Zapasowe rozpoznawanie przez Gemini (audio wysłane w zapytaniu).
pub async fn transcribe_gemini(http: &reqwest::Client, key: &str, model: &str, wav: &[u8], prompt: &str) -> anyhow::Result<String> {
    let audio = base64::engine::general_purpose::STANDARD.encode(wav);
    let body = json!({
        "contents": [{
            "role": "user",
            "parts": [
                { "text": format!(
                    "Przepisz dokładnie, co mówi osoba na nagraniu (po polsku). Zwróć wyłącznie tekst wypowiedzi, bez komentarzy. \
                     Jeśli nic nie słychać, zwróć pusty tekst. Możliwe słowa i nicki: {prompt}") },
                { "inlineData": { "mimeType": "audio/wav", "data": audio } }
            ]
        }]
    });
    let resp = http
        .post(format!("{}/models/{model}:generateContent", gemini_base()))
        .header("x-goog-api-key", key)
        .json(&body)
        .send()
        .await
        .context("Brak połączenia z Gemini")?;
    let status = resp.status();
    let value: serde_json::Value = resp.json().await.context("Niezrozumiała odpowiedź Gemini")?;
    if !status.is_success() {
        return Err(anyhow!(super::gemini::describe_error(status.as_u16(), &value)));
    }
    let text = value["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    Ok(clean(&text))
}

/// Whisper przy ciszy potrafi „halucynować” typowe zwroty z napisów.
fn clean(text: &str) -> String {
    let t = text.trim().trim_matches('"').trim();
    let lower = t.to_lowercase();
    const HALLUCINATIONS: &[&str] = &["dziękuję za uwagę", "napisy wykonane przez", "napisy stworzone przez", "subskrybuj", "dzięki za obejrzenie"];
    if HALLUCINATIONS.iter().any(|h| lower.contains(h)) && t.split_whitespace().count() < 8 {
        return String::new();
    }
    t.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_whisper_silence_hallucinations() {
        assert_eq!(clean("Dziękuję za uwagę."), "");
        assert_eq!(clean(" \"Zmień kategorię na GTA 4\" "), "Zmień kategorię na GTA 4");
    }

    #[test]
    fn prompt_respects_length_limit() {
        let names: Vec<String> = (0..500).map(|i| format!("widz_numer_{i}")).collect();
        assert!(build_prompt(&names).len() <= MAX_PROMPT_CHARS);
    }
}
