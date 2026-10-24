//! Własny model AI przez API zgodne z OpenAI (`/chat/completions` z narzędziami).
//!
//! Działa z OpenAI, OpenRouter, Groq, a lokalnie z Ollamą i LM Studio
//! (bez klucza). Historia rozmowy jest w formacie OpenAI: wiadomość
//! asystenta z `tool_calls`, potem po jednej wiadomości `tool` na wywołanie.

use anyhow::{Context, anyhow};
use serde_json::{Value, json};

use super::gemini::FunctionCall;

/// Gotowe ustawienia popularnych dostawców: (id, nazwa, adres, przykładowy model, czy wymaga klucza).
pub const PRESETS: &[(&str, &str, &str, &str, bool)] = &[
    ("openai", "OpenAI", "https://api.openai.com/v1", "gpt-4.1-mini", true),
    ("openrouter", "OpenRouter (setki modeli)", "https://openrouter.ai/api/v1", "openai/gpt-4.1-mini", true),
    ("groq", "Groq (darmowy limit)", "https://api.groq.com/openai/v1", "llama-3.3-70b-versatile", true),
    ("ollama", "Ollama (lokalnie, za darmo)", "http://localhost:11434/v1", "qwen2.5:7b", false),
    ("lmstudio", "LM Studio (lokalnie, za darmo)", "http://localhost:1234/v1", "", false),
    ("custom", "Własny adres (zgodny z OpenAI)", "", "", false),
];

pub fn preset(id: &str) -> Option<&'static (&'static str, &'static str, &'static str, &'static str, bool)> {
    PRESETS.iter().find(|p| p.0 == id)
}

/// Adres API dla presetu (dla „custom” — wpisany przez użytkownika).
pub fn base_url(preset_id: &str, custom: &str) -> String {
    match preset(preset_id) {
        Some((id, _, url, _, _)) if *id != "custom" => (*url).to_owned(),
        _ => custom.trim().trim_end_matches('/').to_owned(),
    }
}

/// Czy adres wskazuje na ten komputer (Ollama, LM Studio) — wtedy klucz nie jest potrzebny.
pub fn is_local(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    matches!(host, "localhost" | "127.0.0.1" | "[") || rest.starts_with("[::1]")
}

/// Adres musi być http(s); zwykłe http tylko na tym komputerze (klucz nie poleci otwartym tekstem).
pub fn validate_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("Wpisz adres API (np. http://localhost:11434/v1).".into());
    }
    if url.starts_with("https://") || (url.starts_with("http://") && is_local(url)) {
        Ok(())
    } else {
        Err("Adres musi zaczynać się od https:// (albo http://localhost dla modeli lokalnych).".into())
    }
}

pub struct Endpoint<'a> {
    pub base: &'a str,
    pub key: Option<&'a str>,
    pub model: &'a str,
}

/// Deklaracje narzędzi (format Gemini = JSON Schema) → format OpenAI.
pub fn tools(declarations: &Value) -> Value {
    let list: Vec<Value> = declarations
        .as_array()
        .map(|d| {
            d.iter()
                .map(|f| {
                    let params = f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                    json!({ "type": "function", "function": { "name": f["name"], "description": f["description"], "parameters": params } })
                })
                .collect()
        })
        .unwrap_or_default();
    Value::Array(list)
}

pub async fn chat(http: &reqwest::Client, ep: &Endpoint<'_>, system: &str, history: &[Value], tools: &Value) -> anyhow::Result<Value> {
    let mut messages = vec![json!({ "role": "system", "content": system })];
    messages.extend(history.iter().cloned());
    let body = json!({
        "model": ep.model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "temperature": 0.4,
        "max_tokens": 1024,
    });
    let mut req = http.post(format!("{}/chat/completions", ep.base)).json(&body);
    if let Some(key) = ep.key {
        req = req.bearer_auth(key);
    }
    if ep.base.contains("openrouter.ai") {
        req = req.header("X-Title", "LiMBOCHAT");
    }
    let resp = req.send().await.map_err(|e| anyhow!(connection_error(ep.base, &e)))?;
    let status = resp.status();
    let raw = resp.text().await.context("Brak odpowiedzi od modelu")?;
    let value: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "error": { "message": raw.chars().take(200).collect::<String>() } }));
    if !status.is_success() {
        return Err(anyhow!(describe_error(status.as_u16(), &value)));
    }
    let message = &value["choices"][0]["message"];
    if message.is_null() {
        let detail = value["error"]["message"].as_str().unwrap_or("pusta odpowiedź");
        return Err(anyhow!("Model nie odpowiedział ({detail})."));
    }
    Ok(normalize(message))
}

/// Jedno pytanie bez narzędzi (np. rymowanka) — sam tekst odpowiedzi.
pub async fn complete(http: &reqwest::Client, ep: &Endpoint<'_>, system: &str, prompt: &str) -> anyhow::Result<String> {
    let body = json!({
        "model": ep.model,
        "messages": [{ "role": "system", "content": system }, { "role": "user", "content": prompt }],
        "temperature": 1.0,
        "max_tokens": 200,
    });
    let mut req = http.post(format!("{}/chat/completions", ep.base)).json(&body);
    if let Some(key) = ep.key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.map_err(|e| anyhow!(connection_error(ep.base, &e)))?;
    let status = resp.status();
    let value: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(anyhow!(describe_error(status.as_u16(), &value)));
    }
    Ok(text(&value["choices"][0]["message"]))
}

/// Do historii trafia tylko to, co każdy serwer przyjmie z powrotem.
fn normalize(message: &Value) -> Value {
    let mut out = json!({ "role": "assistant", "content": message["content"].as_str().unwrap_or_default() });
    if let Some(calls) = message["tool_calls"].as_array().filter(|c| !c.is_empty()) {
        let calls: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let id = c["id"].as_str().filter(|s| !s.is_empty()).map_or_else(|| format!("call_{i}"), str::to_owned);
                let args = match &c["function"]["arguments"] {
                    Value::String(s) => s.clone(),
                    Value::Null => "{}".to_owned(),
                    other => other.to_string(),
                };
                json!({ "id": id, "type": "function", "function": { "name": c["function"]["name"], "arguments": args } })
            })
            .collect();
        out["tool_calls"] = Value::Array(calls);
    }
    out
}

pub fn tool_calls(message: &Value) -> Vec<FunctionCall> {
    message["tool_calls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .filter_map(|c| {
                    let name = c["function"]["name"].as_str()?.to_owned();
                    let args = c["function"]["arguments"]
                        .as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}));
                    Some(FunctionCall { id: c["id"].as_str().map(str::to_owned), name, args })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Tekst odpowiedzi bez „myślenia” modeli rozumujących (`<think>…</think>`).
pub fn text(message: &Value) -> String {
    let mut s = message["content"].as_str().unwrap_or_default().to_owned();
    while let Some(start) = s.find("<think>") {
        match s[start..].find("</think>") {
            Some(end) => s.replace_range(start..start + end + "</think>".len(), ""),
            None => s.truncate(start),
        }
    }
    s.trim().to_owned()
}

pub fn user_text(text: &str) -> Value {
    json!({ "role": "user", "content": text })
}

pub fn tool_results(results: &[(FunctionCall, Value)]) -> Vec<Value> {
    results
        .iter()
        .enumerate()
        .map(|(i, (call, result))| {
            let id = call.id.clone().unwrap_or_else(|| format!("call_{i}"));
            json!({ "role": "tool", "tool_call_id": id, "name": call.name, "content": result.to_string() })
        })
        .collect()
}

/// Lista modeli z `/models` (do podpowiedzi w ustawieniach).
pub async fn list_models(http: &reqwest::Client, base: &str, key: Option<&str>) -> Result<Vec<String>, String> {
    let mut req = http.get(format!("{base}/models"));
    if let Some(key) = key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.map_err(|e| connection_error(base, &e))?;
    let status = resp.status();
    let value: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(describe_error(status.as_u16(), &value));
    }
    let mut models: Vec<String> = value["data"]
        .as_array()
        .or_else(|| value["models"].as_array())
        .map(|list| list.iter().filter_map(|m| m["id"].as_str().or_else(|| m["name"].as_str()).map(str::to_owned)).collect())
        .unwrap_or_default();
    models.sort();
    models.dedup();
    Ok(models)
}

fn connection_error(base: &str, e: &reqwest::Error) -> String {
    if is_local(base) {
        if base.contains(":11434") {
            return "Nie mogę połączyć się z Ollamą — czy jest uruchomiona? (ollama serve)".into();
        }
        if base.contains(":1234") {
            return "Nie mogę połączyć się z LM Studio — włącz serwer w zakładce Developer.".into();
        }
        return format!("Nie mogę połączyć się z {base} — czy serwer modelu działa?");
    }
    if e.is_timeout() {
        "Model nie odpowiedział na czas — spróbuj ponownie.".into()
    } else {
        format!("Brak połączenia z {base}.")
    }
}

pub fn describe_error(status: u16, body: &Value) -> String {
    let message = body["error"]["message"].as_str().or_else(|| body["error"].as_str()).unwrap_or_default();
    let lower = message.to_lowercase();
    if lower.contains("tool") && (lower.contains("support") || lower.contains("not available")) {
        return "Ten model nie obsługuje narzędzi (function calling) — wybierz inny, np. qwen2.5, llama3.1, gpt-4.1-mini.".into();
    }
    match status {
        401 => "Klucz API własnego modelu jest nieprawidłowy — sprawdź go w ustawieniach.".into(),
        402 => "Brak środków na koncie dostawcy modelu.".into(),
        403 => "Klucz nie ma dostępu do tego modelu.".into(),
        404 => {
            if lower.contains("model") || message.is_empty() {
                "Nie ma takiego modelu — sprawdź nazwę modelu w ustawieniach (przycisk „Pobierz listę”).".into()
            } else {
                format!("Adres API nie odpowiada jak OpenAI (404): {message}")
            }
        }
        429 => "Limit dostawcy modelu wyczerpany — spróbuj za chwilę.".into(),
        500..=599 => "Serwer modelu ma problem — spróbuj ponownie.".into(),
        _ if message.is_empty() => format!("Model zwrócił błąd HTTP {status}."),
        _ => format!("Model: {message} (HTTP {status})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_declarations_and_parses_calls() {
        let t = tools(&json!([{ "name": "clear_chat", "description": "x" }, { "name": "set_title", "description": "y", "parameters": { "type": "object", "properties": { "title": { "type": "string" } } } }]));
        assert_eq!(t[0]["function"]["parameters"]["type"], "object");
        assert_eq!(t[1]["function"]["parameters"]["properties"]["title"]["type"], "string");

        let msg = normalize(&json!({
            "role": "assistant", "content": null, "reasoning": "hmm",
            "tool_calls": [{ "id": "", "type": "function", "function": { "name": "set_title", "arguments": "{\"title\":\"GTA\"}" } },
                           { "function": { "name": "clear_chat", "arguments": "nie-json" } }]
        }));
        assert!(msg.get("reasoning").is_none(), "pola specyficzne dla dostawcy nie wracają do historii");
        let calls = tool_calls(&msg);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_deref(), Some("call_0"));
        assert_eq!(calls[0].args["title"], "GTA");
        assert_eq!(calls[1].args, json!({}));
        let results = tool_results(&[(calls.into_iter().next().unwrap(), json!({ "ok": true }))]);
        assert_eq!(results[0]["tool_call_id"], "call_0");
        assert_eq!(results[0]["role"], "tool");
    }

    #[test]
    fn strips_thinking() {
        assert_eq!(text(&json!({ "content": "<think>hmm\nok</think>\nGotowe." })), "Gotowe.");
        assert_eq!(text(&json!({ "content": "Jasne. <think>niedokończone" })), "Jasne.");
    }

    #[test]
    fn urls() {
        assert!(is_local("http://localhost:11434/v1") && is_local("http://127.0.0.1:1234/v1") && is_local("http://[::1]:8080/v1"));
        assert!(!is_local("https://api.openai.com/v1") && !is_local("http://localhost.evil.com/v1"));
        assert!(validate_url("http://localhost:11434/v1").is_ok());
        assert!(validate_url("http://example.com/v1").is_err(), "klucz nie poleci zwykłym http");
        assert!(validate_url("https://example.com/v1").is_ok());
        assert_eq!(base_url("groq", "ignored"), "https://api.groq.com/openai/v1");
        assert_eq!(base_url("custom", " https://x.ai/v1/ "), "https://x.ai/v1");
    }

    #[test]
    fn friendly_errors() {
        let e = describe_error(400, &json!({ "error": { "message": "registry.ollama.ai/library/gemma:2b does not support tools" } }));
        assert!(e.contains("nie obsługuje narzędzi"));
        assert!(describe_error(401, &json!({})).contains("Klucz"));
    }
}
