//! Obrazki z linków na czacie (kappa.lol) — pobierane przez silnik.
//!
//! Panel pokazuje `<img src="/media?u=…">`, a silnik pobiera plik sam:
//! * tylko z dozwolonych serwisów (kappa.lol) i tylko przez https,
//! * tylko obrazy i krótkie wideo (png, jpg, gif, webp, avif, mp4, webm), do 25 MB,
//! * jeśli link prowadzi do strony, bierzemy z niej `og:image` / `og:video`,
//! * adres IP streamera nie trafia do serwisu z przeglądarki panelu, a pliki
//!   są w pamięci podręcznej (ten sam obrazek nie pobiera się dwa razy).

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::Duration,
};

use axum::body::Bytes;
use reqwest::{Url, redirect::Policy};

const MAX_BYTES: usize = 25 * 1024 * 1024;
const MAX_HTML: usize = 512 * 1024;
const CACHE_BYTES: usize = 80 * 1024 * 1024;
const ALLOWED_TYPES: [&str; 7] = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/avif", "video/mp4", "video/webm"];

/// Czy panel może pokazać podgląd tego linku.
pub fn allowed(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    url.scheme() == "https" && (host == "kappa.lol" || host.ends_with(".kappa.lol"))
}

/// Link z czatu → adres https (czat często pomija „https://”).
pub fn normalize(raw: &str) -> Option<Url> {
    let raw = raw.trim();
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") { raw.to_owned() } else { format!("https://{raw}") };
    let mut url = Url::parse(&with_scheme).ok()?;
    if url.scheme() == "http" {
        url.set_scheme("https").ok()?;
    }
    allowed(&url).then_some(url)
}

pub struct Media {
    http: reqwest::Client,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    items: HashMap<String, (Bytes, &'static str)>,
    order: VecDeque<String>,
    size: usize,
}

impl Default for Media {
    fn default() -> Self {
        Self::new()
    }
}

impl Media {
    pub fn new() -> Self {
        // Przekierowania: najwyżej 3, tylko https i bez adresów lokalnych.
        let policy = Policy::custom(|attempt| {
            let url = attempt.url();
            let host = url.host_str().unwrap_or_default();
            let local = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok();
            if attempt.previous().len() >= 3 || url.scheme() != "https" || local {
                attempt.stop()
            } else {
                attempt.follow()
            }
        });
        let http = reqwest::Client::builder()
            .redirect(policy)
            .user_agent("LiMBOCHAT (podgląd obrazków na czacie)")
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self { http, cache: Mutex::new(Cache::default()) }
    }

    /// Plik dla panelu: (treść, typ). `Err` = komunikat do logu.
    pub async fn get(&self, raw: &str, demo: bool) -> Result<(Bytes, &'static str), String> {
        let url = normalize(raw).ok_or("niedozwolony adres")?;
        if let Some(hit) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).items.get(url.as_str()) {
            return Ok(hit.clone());
        }
        let result = if demo && url.path().starts_with("/demo") {
            Ok((Bytes::from_static(crate::demo::KAPPA_DEMO), "image/png"))
        } else {
            self.fetch(&url, 0).await
        }?;
        self.store(url.as_str(), &result);
        Ok(result)
    }

    async fn fetch(&self, url: &Url, depth: u8) -> Result<(Bytes, &'static str), String> {
        let target = test_rewrite(url);
        let mut resp = self.http.get(target).send().await.map_err(|e| format!("pobieranie: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("serwer odpowiedział {}", resp.status().as_u16()));
        }
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let html = ctype == "text/html";
        let kind = ALLOWED_TYPES.iter().copied().find(|t| *t == ctype);
        if kind.is_none() && !html {
            return Err(format!("nieobsługiwany typ pliku: {ctype}"));
        }
        if resp.content_length().is_some_and(|l| l as usize > MAX_BYTES) {
            return Err("plik za duży".into());
        }
        let limit = if html { MAX_HTML } else { MAX_BYTES };
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("pobieranie: {e}"))? {
            body.extend_from_slice(&chunk);
            if body.len() > limit {
                return Err("plik za duży".into());
            }
        }
        match kind {
            Some(kind) => Ok((Bytes::from(body), kind)),
            None => {
                // Strona z obrazkiem: bierzemy og:video albo og:image (tylko z dozwolonego serwisu).
                if depth > 0 {
                    return Err("strona zamiast obrazka".into());
                }
                let page = String::from_utf8_lossy(&body);
                let found = meta_content(&page, "og:video").or_else(|| meta_content(&page, "og:image")).ok_or("strona bez obrazka")?;
                let next = url.join(&found).map_err(|_| "zły adres obrazka")?;
                if !allowed(&next) {
                    return Err("obrazek z niedozwolonego serwisu".into());
                }
                Box::pin(self.fetch(&next, depth + 1)).await
            }
        }
    }

    fn store(&self, key: &str, value: &(Bytes, &'static str)) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.size += value.0.len();
        cache.items.insert(key.to_owned(), value.clone());
        cache.order.push_back(key.to_owned());
        while cache.size > CACHE_BYTES {
            let Some(old) = cache.order.pop_front() else { break };
            if let Some((bytes, _)) = cache.items.remove(&old) {
                cache.size -= bytes.len();
            }
        }
    }
}

/// `<meta property="og:image" content="…">` (kolejność atrybutów dowolna).
fn meta_content(html: &str, property: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("<meta") {
        let start = from + pos;
        let end = start + lower[start..].find('>')?;
        let tag = &html[start..end];
        let tag_lower = &lower[start..end];
        if tag_lower.contains(&format!("\"{property}\"")) || tag_lower.contains(&format!("'{property}'")) {
            let i = tag_lower.find("content=")? + "content=".len();
            let quote = tag[i..].chars().next()?;
            if quote == '"' || quote == '\'' {
                let rest = &tag[i + 1..];
                return rest.find(quote).map(|j| rest[..j].replace("&amp;", "&"));
            }
        }
        from = end;
    }
    None
}

/// Testy: `LIMBOCHAT_MEDIA_BASE=http://127.0.0.1:port` podmienia https://kappa.lol.
fn test_rewrite(url: &Url) -> String {
    match std::env::var("LIMBOCHAT_MEDIA_BASE") {
        Ok(base) => format!("{}{}{}", base.trim_end_matches('/'), url.path(), url.query().map(|q| format!("?{q}")).unwrap_or_default()),
        Err(_) => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_kappa_over_https() {
        assert!(normalize("kappa.lol/AbCd.png").is_some());
        assert_eq!(normalize("http://kappa.lol/x.gif").unwrap().scheme(), "https");
        assert!(normalize("https://i.kappa.lol/x.png").is_some());
        assert!(normalize("https://kappa.lol.evil.com/x.png").is_none());
        assert!(normalize("https://evilkappa.lol/x.png").is_none());
        assert!(normalize("https://imgur.com/x.png").is_none());
        assert!(normalize("javascript:alert(1)").is_none());
    }

    #[test]
    fn finds_open_graph_image() {
        let html = r#"<html><head><meta name="x"><meta content='https://kappa.lol/i/a.webp' property="og:image"><meta property="og:title" content="t"></head>"#;
        assert_eq!(meta_content(html, "og:image").as_deref(), Some("https://kappa.lol/i/a.webp"));
        assert_eq!(meta_content(html, "og:video"), None);
    }
}
