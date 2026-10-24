//! Klient Gemini (generateContent) z wywoływaniem funkcji.
//!
//! Treść odpowiedzi modelu przechowujemy jako surowy JSON i odsyłamy ją
//! w historii bez zmian — modele Gemini 3 dołączają do części z wywołaniem
//! funkcji „podpis myśli” (`thoughtSignature`), który musi wrócić do API
//! w nienaruszonej postaci.

use anyhow::{Context, anyhow};
use serde_json::{Value, json};

use super::stt::gemini_base;

pub struct FunctionCall {
    pub id: Option<String>,
    pub name: String,
    pub args: Value,
}

pub async fn generate(
    http: &reqwest::Client,
    key: &str,
    model: &str,
    system: &str,
    contents: &[Value],
    declarations: &Value,
) -> anyhow::Result<Value> {
    let body = json!({
        "systemInstruction": { "parts": [{ "text": system }] },
        "contents": contents,
        "tools": [{ "functionDeclarations": declarations }],
        "generationConfig": { "maxOutputTokens": 2048 },
    });
    let resp = http
        .post(format!("{}/models/{model}:generateContent", gemini_base()))
        .header("x-goog-api-key", key)
        .json(&body)
        .send()
        .await
        .context("Brak połączenia z Gemini")?;
    let status = resp.status();
    let value: Value = resp.json().await.context("Niezrozumiała odpowiedź Gemini")?;
    if !status.is_success() {
        return Err(anyhow!(describe_error(status.as_u16(), &value)));
    }

    if let Some(reason) = value["promptFeedback"]["blockReason"].as_str() {
        return Err(anyhow!("Gemini odrzuciło polecenie ({reason})."));
    }
    let content = value["candidates"][0]["content"].clone();
    if content.is_null() || content["parts"].as_array().is_none_or(|p| p.is_empty()) {
        let reason = value["candidates"][0]["finishReason"].as_str().unwrap_or("brak treści");
        return Err(anyhow!("Gemini nie odpowiedziało ({reason})."));
    }
    Ok(normalize_content(content))
}

/// Jedno pytanie bez narzędzi (np. rymowanka) — sam tekst odpowiedzi.
pub async fn generate_text(http: &reqwest::Client, key: &str, model: &str, system: &str, prompt: &str) -> anyhow::Result<String> {
    let body = json!({
        "systemInstruction": { "parts": [{ "text": system }] },
        "contents": [user_text(prompt)],
        "generationConfig": { "maxOutputTokens": 512, "temperature": 1.0 },
    });
    let resp = http
        .post(format!("{}/models/{model}:generateContent", gemini_base()))
        .header("x-goog-api-key", key)
        .json(&body)
        .send()
        .await
        .context("Brak połączenia z Gemini")?;
    let status = resp.status();
    let value: Value = resp.json().await.context("Niezrozumiała odpowiedź Gemini")?;
    if !status.is_success() {
        return Err(anyhow!(describe_error(status.as_u16(), &value)));
    }
    Ok(text(&value["candidates"][0]["content"]))
}

/// Upewnia się, że treść ma rolę „model” (wymagane przy odsyłaniu historii).
fn normalize_content(mut content: Value) -> Value {
    if content["role"].as_str() != Some("model") {
        content["role"] = json!("model");
    }
    content
}

pub fn function_calls(content: &Value) -> Vec<FunctionCall> {
    content["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| {
                    let call = p.get("functionCall")?;
                    Some(FunctionCall {
                        id: call["id"].as_str().map(str::to_owned),
                        name: call["name"].as_str()?.to_owned(),
                        args: call.get("args").cloned().unwrap_or_else(|| json!({})),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Tekst odpowiedzi (bez części „myśli”).
pub fn text(content: &Value) -> String {
    content["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|p| !p["thought"].as_bool().unwrap_or(false))
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
        .trim()
        .to_owned()
}

pub fn user_text(text: &str) -> Value {
    json!({ "role": "user", "parts": [{ "text": text }] })
}

/// Odpowiedzi na wszystkie wywołania z jednej tury idą w jednej wiadomości.
pub fn function_responses(results: &[(FunctionCall, Value)]) -> Value {
    let parts: Vec<Value> = results
        .iter()
        .map(|(call, response)| {
            let mut fr = json!({ "name": call.name, "response": response });
            if let Some(id) = &call.id {
                fr["id"] = json!(id);
            }
            json!({ "functionResponse": fr })
        })
        .collect();
    json!({ "role": "user", "parts": parts })
}

pub fn describe_error(status: u16, body: &Value) -> String {
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let lower = message.to_lowercase();
    match status {
        400 if lower.contains("api key") => "Klucz Gemini jest nieprawidłowy — sprawdź go w ustawieniach LimboAI.".into(),
        401 | 403 => "Klucz Gemini nie ma dostępu do tego modelu — sprawdź klucz w ustawieniach.".into(),
        404 => "Wybrany model Gemini jest niedostępny — zmień go w ustawieniach LimboAI.".into(),
        429 => "Limit Gemini wyczerpany — spróbuj za chwilę.".into(),
        500..=599 => "Gemini jest chwilowo przeciążone — spróbuj ponownie.".into(),
        _ => format!("Gemini: {message} (HTTP {status})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_calls_and_keeps_signatures_in_history() {
        let content = json!({
            "role": "model",
            "parts": [
                { "functionCall": { "id": "c1", "name": "set_category", "args": { "name": "Grand Theft Auto IV" } }, "thoughtSignature": "SIG" },
                { "functionCall": { "name": "get_stream_info" } }
            ]
        });
        let calls = function_calls(&content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_deref(), Some("c1"));
        assert_eq!(calls[0].args["name"], "Grand Theft Auto IV");
        assert_eq!(calls[1].args, json!({}));

        let reply = function_responses(&[(calls.into_iter().next().unwrap(), json!({ "ok": true }))]);
        assert_eq!(reply["parts"][0]["functionResponse"]["id"], "c1");
        assert_eq!(reply["role"], "user");
    }

    #[test]
    fn text_skips_thoughts() {
        let content = json!({ "parts": [{ "text": "myślę…", "thought": true }, { "text": "Gotowe." }] });
        assert_eq!(text(&content), "Gotowe.");
    }

    #[test]
    fn friendly_errors() {
        let body = json!({ "error": { "message": "API key not valid. Please pass a valid API key." } });
        assert!(describe_error(400, &body).contains("nieprawidłowy"));
        assert!(describe_error(429, &json!({})).contains("Limit"));
    }
}
