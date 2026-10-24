//! Klient Twitch Helix API — wysyłanie wiadomości, moderacja, odznaki, profil.
//!
//! Od lutego 2023 komendy IRC (`/ban`, `/timeout`, `/delete`) nie działają;
//! jedyną drogą moderacji jest Helix:
//! * `POST   /moderation/bans`  — ban (bez `duration`) lub timeout (z `duration`)
//! * `DELETE /moderation/bans`  — unban / zdjęcie timeoutu
//! * `DELETE /moderation/chat`  — usunięcie pojedynczej wiadomości
//!
//! Wiadomości wysyłamy przez `POST /chat/messages` (nowoczesny następca
//! `PRIVMSG` przez IRC) — zalogowane połączenie IRC nie jest więc potrzebne.
//!
//! Każde zapytanie wymaga nagłówków `Authorization: Bearer <token>` oraz
//! `Client-Id`, a akcje moderatorskie — `broadcaster_id` (kanał) i
//! `moderator_id` (zalogowany użytkownik). Po 401 odświeżamy token
//! i ponawiamy zapytanie dokładnie raz.

use std::{collections::HashMap, sync::Arc};

use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{AuthError, TokenManager};

const HELIX: &str = "https://api.twitch.tv/helix";
/// Maksymalny timeout dopuszczany przez Twitcha: 2 tygodnie.
pub const MAX_TIMEOUT_SECS: u32 = 1_209_600;
const MAX_REASON_CHARS: usize = 500;
/// Limit długości wiadomości na czacie Twitcha.
pub const MAX_MESSAGE_CHARS: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum HelixError {
    #[error("nie jesteś zalogowany")]
    NotLoggedIn,
    #[error("sesja wygasła — zaloguj się ponownie")]
    SessionExpired,
    #[error("brak uprawnień ({0}). Czy jesteś moderatorem tego kanału?")]
    Forbidden(String),
    #[error("przekroczono limit zapytań Twitcha — spróbuj za {0} s")]
    RateLimited(u64),
    #[error("{message} (HTTP {status})")]
    Api { status: u16, message: String },
    #[error("błąd sieci: {0}")]
    Network(#[from] reqwest::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<AuthError> for HelixError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::NotLoggedIn => Self::NotLoggedIn,
            AuthError::SessionExpired => Self::SessionExpired,
            AuthError::Transient(e) => Self::Other(e),
        }
    }
}

#[derive(Deserialize, Default)]
struct HelixErrorBody {
    #[serde(default)]
    message: String,
}

// ── Odznaki ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BadgeImage {
    pub url: Arc<str>,
    pub title: String,
}

/// Klucz: `"set_id/version"`, np. `"subscriber/3012"`, `"moderator/1"`.
/// Taki sam format ma tag `badges` w wiadomościach IRC.
pub type BadgeMap = HashMap<String, BadgeImage>;

#[derive(Deserialize)]
struct BadgeSetsResponse {
    data: Vec<BadgeSet>,
}

#[derive(Deserialize)]
struct BadgeSet {
    set_id: String,
    versions: Vec<BadgeVersion>,
}

#[derive(Deserialize)]
struct BadgeVersion {
    id: String,
    /// 2x (36 px) rysowane w 18 px — ostre także na ekranach HiDPI.
    image_url_2x: String,
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
struct ModeratedChannelsResponse {
    data: Vec<ModeratedChannel>,
    #[serde(default)]
    pagination: Pagination,
}

#[derive(Deserialize)]
struct ModeratedChannel {
    broadcaster_id: String,
}

#[derive(Deserialize)]
struct DataList<T> {
    data: Vec<T>,
}

#[derive(Deserialize)]
struct SendMessageResult {
    is_sent: bool,
    drop_reason: Option<DropReason>,
}

#[derive(Deserialize)]
struct DropReason {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct UserDto {
    profile_image_url: String,
}

#[derive(Deserialize, Default)]
struct Pagination {
    cursor: Option<String>,
}

// ── Klient ─────────────────────────────────────────────────────────────

pub struct Helix {
    http: reqwest::Client,
    client_id: String,
    tokens: Arc<TokenManager>,
}

impl Helix {
    pub fn new(http: reqwest::Client, client_id: String, tokens: Arc<TokenManager>) -> Self {
        Self { http, client_id, tokens }
    }

    /// ID zalogowanego użytkownika — wartość `moderator_id`.
    pub(crate) async fn me(&self) -> Result<String, HelixError> {
        Ok(self.tokens.user().await.ok_or(HelixError::NotLoggedIn)?.user_id)
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<reqwest::Response, HelixError> {
        for attempt in 0..2 {
            let (token, _) = self.tokens.access_token().await?;
            let mut req = self
                .http
                .request(method.clone(), format!("{HELIX}{path}"))
                .bearer_auth(&token)
                .header("Client-Id", &self.client_id)
                .query(query);
            if let Some(body) = body {
                req = req.json(body);
            }

            let resp = req.send().await?;
            if resp.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.force_refresh(&token).await?;
                continue;
            }
            return check(resp).await;
        }
        Err(HelixError::SessionExpired)
    }

    /// Ban (`duration == None`) albo timeout na `duration` sekund.
    pub async fn ban_user(
        &self,
        broadcaster_id: &str,
        user_id: &str,
        duration: Option<u32>,
        reason: Option<&str>,
    ) -> Result<(), HelixError> {
        let me = self.me().await?;
        let mut data = json!({ "user_id": user_id });
        if let Some(secs) = duration {
            data["duration"] = json!(secs.clamp(1, MAX_TIMEOUT_SECS));
        }
        if let Some(reason) = reason.filter(|r| !r.trim().is_empty()) {
            data["reason"] = json!(reason.chars().take(MAX_REASON_CHARS).collect::<String>());
        }
        self.request(
            Method::POST,
            "/moderation/bans",
            &[("broadcaster_id", broadcaster_id), ("moderator_id", &me)],
            Some(&json!({ "data": data })),
        )
        .await?;
        Ok(())
    }

    pub async fn unban_user(&self, broadcaster_id: &str, user_id: &str) -> Result<(), HelixError> {
        let me = self.me().await?;
        self.request(
            Method::DELETE,
            "/moderation/bans",
            &[("broadcaster_id", broadcaster_id), ("moderator_id", &me), ("user_id", user_id)],
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn delete_message(&self, broadcaster_id: &str, message_id: &str) -> Result<(), HelixError> {
        let me = self.me().await?;
        self.request(
            Method::DELETE,
            "/moderation/chat",
            &[("broadcaster_id", broadcaster_id), ("moderator_id", &me), ("message_id", message_id)],
            None,
        )
        .await?;
        Ok(())
    }

    /// Czy zalogowany użytkownik może moderować kanał (jest jego właścicielem
    /// albo moderatorem). Lista moderowanych kanałów jest stronicowana.
    pub async fn can_moderate(&self, broadcaster_id: &str) -> Result<bool, HelixError> {
        let me = self.me().await?;
        if me == broadcaster_id {
            return Ok(true);
        }

        let mut cursor: Option<String> = None;
        loop {
            let mut query = vec![("user_id", me.as_str()), ("first", "100")];
            if let Some(c) = cursor.as_deref() {
                query.push(("after", c));
            }
            let page: ModeratedChannelsResponse = self
                .request(Method::GET, "/moderation/channels", &query, None)
                .await?
                .json()
                .await?;

            if page.data.iter().any(|c| c.broadcaster_id == broadcaster_id) {
                return Ok(true);
            }
            match page.pagination.cursor {
                Some(next) if !page.data.is_empty() => cursor = Some(next),
                _ => return Ok(false),
            }
        }
    }

    /// Wysyła wiadomość jako zalogowany użytkownik. Twitch może ją odrzucić
    /// (AutoMod, slow mode, tryb tylko emotek, ban…) — wtedy zwracamy powód.
    pub async fn send_chat_message(&self, broadcaster_id: &str, text: &str) -> Result<(), HelixError> {
        let me = self.me().await?;
        let message: String = text.chars().take(MAX_MESSAGE_CHARS).collect();
        let body = json!({ "broadcaster_id": broadcaster_id, "sender_id": me, "message": message });

        let resp: DataList<SendMessageResult> = self
            .request(Method::POST, "/chat/messages", &[], Some(&body))
            .await?
            .json()
            .await?;

        match resp.data.into_iter().next() {
            Some(r) if r.is_sent => Ok(()),
            Some(r) => Err(HelixError::Api {
                status: 200,
                message: r
                    .drop_reason
                    .map(|d| d.message)
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "Twitch odrzucił wiadomość".to_owned()),
            }),
            None => Err(HelixError::Api { status: 200, message: "pusta odpowiedź Twitcha".to_owned() }),
        }
    }

    /// Adres awatara użytkownika (300×300 px).
    pub async fn avatar_url(&self, user_id: &str) -> Result<Option<String>, HelixError> {
        let resp: DataList<UserDto> =
            self.request(Method::GET, "/users", &[("id", user_id)], None).await?.json().await?;
        Ok(resp.data.into_iter().next().map(|u| u.profile_image_url).filter(|u| !u.is_empty()))
    }

    pub async fn global_badges(&self) -> Result<BadgeMap, HelixError> {
        self.badges("/chat/badges/global", &[]).await
    }

    pub async fn channel_badges(&self, broadcaster_id: &str) -> Result<BadgeMap, HelixError> {
        self.badges("/chat/badges", &[("broadcaster_id", broadcaster_id)]).await
    }

    async fn badges(&self, path: &str, query: &[(&str, &str)]) -> Result<BadgeMap, HelixError> {
        let resp: BadgeSetsResponse = self.request(Method::GET, path, query, None).await?.json().await?;
        Ok(resp
            .data
            .into_iter()
            .flat_map(|set| {
                set.versions.into_iter().map(move |v| {
                    (format!("{}/{}", set.set_id, v.id), BadgeImage { url: v.image_url_2x.into(), title: v.title })
                })
            })
            .collect())
    }
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response, HelixError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }

    let reset_in = resp
        .headers()
        .get("Ratelimit-Reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .map(|ts| (ts - chrono::Utc::now().timestamp()).max(1) as u64)
        .unwrap_or(1);

    let message = resp.json::<HelixErrorBody>().await.unwrap_or_default().message;
    Err(match status {
        StatusCode::UNAUTHORIZED => HelixError::SessionExpired,
        StatusCode::FORBIDDEN => HelixError::Forbidden(message),
        StatusCode::TOO_MANY_REQUESTS => HelixError::RateLimited(reset_in),
        s => HelixError::Api { status: s.as_u16(), message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_keys_match_irc_badge_tag_format() {
        let json = r#"{"data":[{"set_id":"subscriber","versions":[
            {"id":"3012","image_url_1x":"a","image_url_2x":"https://x/2x","image_url_4x":"c","title":"Tier 3, 1 rok"}]}]}"#;
        let resp: BadgeSetsResponse = serde_json::from_str(json).unwrap();
        let map: BadgeMap = resp
            .data
            .into_iter()
            .flat_map(|set| set.versions.into_iter().map(move |v| (format!("{}/{}", set.set_id, v.id), v)))
            .map(|(k, v)| (k, BadgeImage { url: v.image_url_2x.into(), title: v.title }))
            .collect();
        assert_eq!(&*map["subscriber/3012"].url, "https://x/2x");
    }
}
