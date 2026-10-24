//! Klient API 7TV v3.
//!
//! Endpointy:
//! * `GET https://7tv.io/v3/emote-sets/global`        — emotki globalne
//! * `GET https://7tv.io/v3/users/twitch/{twitch_id}` — konto 7TV kanału
//!   wraz z aktywnym zestawem emotek (`emote_set`); 404 = kanał bez 7TV.
//!
//! Każda emotka ma `host.url` (np. `//cdn.7tv.app/emote/<id>`) oraz listę
//! plików (`1x.webp`, `2x.gif`, `4x.avif`…). Wybieramy plik według skali
//! i preferowanego formatu.

use anyhow::Context;
use async_trait::async_trait;
use serde::Deserialize;

use crate::emotes::{Emote, EmoteProvider, EmoteScale, EmoteSource};

const API_BASE: &str = "https://7tv.io/v3";

/// Flaga aktywnej emotki w zestawie: nakładka zero-width.
const ACTIVE_FLAG_ZERO_WIDTH: u32 = 1 << 0;
/// Flaga samej emotki (ustawiona przez autora): zero-width.
const EMOTE_FLAG_ZERO_WIDTH: u32 = 1 << 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)] // opcja konfiguracji wybierana w BackendConfig
pub enum ImageFormat {
    /// WebP: mniejsze pliki, animacje obsługiwane przez loader egui. Domyślny.
    #[default]
    Webp,
    /// GIF dla animowanych emotek (gorsza jakość, ale najszerzej wspierany).
    GifForAnimated,
}

// ── Modele odpowiedzi (tylko pola, których używamy) ────────────────────

#[derive(Debug, Deserialize)]
struct EmoteSetDto {
    #[serde(default)]
    emotes: Option<Vec<ActiveEmoteDto>>,
}

#[derive(Debug, Deserialize)]
struct ActiveEmoteDto {
    id: String,
    /// Alias w zestawie — to tę nazwę ludzie wpisują w czacie.
    name: String,
    #[serde(default)]
    flags: u32,
    data: Option<EmoteDataDto>,
}

#[derive(Debug, Deserialize)]
struct EmoteDataDto {
    #[serde(default)]
    animated: bool,
    #[serde(default)]
    flags: u32,
    host: Option<ImageHostDto>,
}

#[derive(Debug, Deserialize)]
struct ImageHostDto {
    url: String,
    #[serde(default)]
    files: Vec<ImageFileDto>,
}

#[derive(Debug, Deserialize)]
struct ImageFileDto {
    name: String,
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
struct UserConnectionDto {
    emote_set: Option<EmoteSetDto>,
}

// ── Klient ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SevenTvClient {
    http: reqwest::Client,
    scale: EmoteScale,
    format: ImageFormat,
}

impl SevenTvClient {
    pub fn new(http: reqwest::Client, scale: EmoteScale, format: ImageFormat) -> Self {
        Self { http, scale, format }
    }

    fn convert_set(&self, set: EmoteSetDto) -> Vec<Emote> {
        set.emotes
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| self.convert_emote(e))
            .collect()
    }

    fn convert_emote(&self, active: ActiveEmoteDto) -> Option<Emote> {
        let data = active.data?;
        let host = data.host?;
        let prefix = self.scale.seventv_prefix();

        let wanted_ext = match (self.format, data.animated) {
            (ImageFormat::GifForAnimated, true) => "gif",
            _ => "webp",
        };
        let wanted = format!("{prefix}.{wanted_ext}");

        // Najpierw dokładne dopasowanie, potem dowolny plik w tej skali
        // (np. gdy emotka nie ma wariantu webp).
        let file = host
            .files
            .iter()
            .find(|f| f.name == wanted)
            .or_else(|| host.files.iter().find(|f| f.name.starts_with(prefix) && !f.name.contains("static")))?;

        // `host.url` jest protocol-relative: "//cdn.7tv.app/emote/<id>"
        let base = host.url.strip_prefix("//").unwrap_or(&host.url);
        let factor = self.scale.factor();

        Some(Emote {
            id: active.id.into(),
            name: active.name.into(),
            url: format!("https://{base}/{}", file.name).into(),
            width: file.width as f32 / factor,
            height: file.height as f32 / factor,
            animated: data.animated,
            zero_width: active.flags & ACTIVE_FLAG_ZERO_WIDTH != 0
                || data.flags & EMOTE_FLAG_ZERO_WIDTH != 0,
            provider: EmoteProvider::SevenTv,
        })
    }
}

#[async_trait]
impl EmoteSource for SevenTvClient {
    fn name(&self) -> &'static str {
        "7TV"
    }

    async fn global_emotes(&self) -> anyhow::Result<Vec<Emote>> {
        let set: EmoteSetDto = self
            .http
            .get(format!("{API_BASE}/emote-sets/global"))
            .send()
            .await
            .context("7TV: błąd połączenia (global)")?
            .error_for_status()
            .context("7TV: błędny status (global)")?
            .json()
            .await
            .context("7TV: niepoprawny JSON (global)")?;
        Ok(self.convert_set(set))
    }

    async fn channel_emotes(&self, twitch_channel_id: &str) -> anyhow::Result<Vec<Emote>> {
        let resp = self
            .http
            .get(format!("{API_BASE}/users/twitch/{twitch_channel_id}"))
            .send()
            .await
            .context("7TV: błąd połączenia (kanał)")?;

        // Kanał bez konta 7TV to normalna sytuacja, nie błąd.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }

        let user: UserConnectionDto = resp
            .error_for_status()
            .context("7TV: błędny status (kanał)")?
            .json()
            .await
            .context("7TV: niepoprawny JSON (kanał)")?;

        Ok(user.emote_set.map(|s| self.convert_set(s)).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "id": "set1",
        "emotes": [
            { "id": "a1", "name": "KEKW", "flags": 0,
              "data": { "animated": false, "flags": 0,
                "host": { "url": "//cdn.7tv.app/emote/a1",
                  "files": [ {"name":"1x.webp","width":32,"height":32,"format":"WEBP"},
                             {"name":"2x.webp","width":64,"height":64,"format":"WEBP"} ] } } },
            { "id": "b2", "name": "RainTime", "flags": 1,
              "data": { "animated": true, "flags": 256,
                "host": { "url": "//cdn.7tv.app/emote/b2",
                  "files": [ {"name":"1x.gif","width":28,"height":28,"format":"GIF"},
                             {"name":"1x.webp","width":28,"height":28,"format":"WEBP"},
                             {"name":"2x.webp","width":56,"height":56,"format":"WEBP"} ] } } },
            { "id": "c3", "name": "Broken", "data": null }
        ]
    }"#;

    #[test]
    fn parses_emote_set_and_builds_urls() {
        let client = SevenTvClient::new(reqwest::Client::new(), EmoteScale::X2, ImageFormat::Webp);
        let set: EmoteSetDto = serde_json::from_str(SAMPLE).unwrap();
        let emotes = client.convert_set(set);

        assert_eq!(emotes.len(), 2, "emotka bez danych powinna zostać pominięta");
        let kekw = &emotes[0];
        assert_eq!(&*kekw.url, "https://cdn.7tv.app/emote/a1/2x.webp");
        assert_eq!(kekw.height, 32.0, "rozmiar logiczny = rozmiar pliku / skala");
        assert!(!kekw.zero_width);
        assert!(emotes[1].zero_width);
    }

    #[test]
    fn prefers_gif_for_animated_when_requested() {
        let client = SevenTvClient::new(reqwest::Client::new(), EmoteScale::X1, ImageFormat::GifForAnimated);
        let set: EmoteSetDto = serde_json::from_str(SAMPLE).unwrap();
        let emotes = client.convert_set(set);
        assert!(emotes[1].url.ends_with("1x.gif"));
        assert!(emotes[0].url.ends_with("1x.webp"), "statyczne zostają w webp");
    }
}
