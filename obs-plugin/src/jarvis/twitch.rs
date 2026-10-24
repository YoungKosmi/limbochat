//! Endpointy Helix potrzebne Jarvisowi (ustawienia streamu, tryby czatu,
//! raid, klip, marker, reklama, ankiety, statystyki).
//!
//! Rozszerzamy istniejący typ `Helix`, więc korzystamy z tej samej sesji
//! (odświeżanie tokena, ponowienie po 401, czytelne błędy).

use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::helix::{Helix, HelixError};

/// Uprawnienia, o które wtyczka prosi dodatkowo (ponad czat i moderację).
pub const EXTRA_SCOPES: &[&str] = &[
    "channel:manage:broadcast",       // kategoria, tytuł, markery
    "moderator:manage:chat_settings", // slow mode, tryb emotek, followers-only…
    "moderator:manage:announcements", // ogłoszenia na czacie
    "moderator:manage:shoutouts",     // shoutout
    "channel:manage:raids",           // raid
    "clips:edit",                     // klipy
    "channel:edit:commercial",        // reklamy
    "channel:manage:polls",           // ankiety
    "channel:manage:predictions",     // predykcje (zakłady punktami kanału)
    "channel:manage:moderators",      // nadawanie / odbieranie moderatora
    "channel:manage:vips",            // nadawanie / odbieranie VIP
    "moderator:manage:warnings",      // ostrzeżenia
    "moderator:manage:shield_mode",   // tryb tarczy
    "user:manage:chat_color",         // kolor nicku na czacie
    "moderator:read:followers",       // alerty follow (EventSub)
    "channel:read:redemptions",       // alerty nagród za punkty kanału (EventSub)
];

/// Kolory nicku dostępne dla każdego (hex tylko z Prime/Turbo).
pub const CHAT_COLORS: &[&str] = &[
    "blue", "blue_violet", "cadet_blue", "chocolate", "coral", "dodger_blue", "firebrick", "golden_rod",
    "green", "hot_pink", "orange_red", "red", "sea_green", "spring_green", "yellow_green",
];

#[derive(Debug, Deserialize)]
struct Data<T> {
    data: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Category {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelInfo {
    pub title: String,
    pub game_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamInfo {
    pub viewer_count: u64,
    pub started_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TwitchUser {
    pub id: String,
    pub login: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
struct Followers {
    total: u64,
}

#[derive(Debug, Deserialize)]
struct Clip {
    id: String,
}

#[derive(Debug, Deserialize)]
struct Commercial {
    length: u32,
    #[serde(default)]
    retry_after: u32,
}

#[derive(Debug, Deserialize)]
struct Prediction {
    id: String,
    title: String,
    status: String,
    outcomes: Vec<PredictionOutcome>,
}

#[derive(Debug, Deserialize)]
struct PredictionOutcome {
    id: String,
    title: String,
}

/// Jak zakończyć predykcję.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictionEnd {
    Lock,
    Resolve,
    Cancel,
}

#[derive(Debug, Deserialize)]
struct Poll {
    id: String,
    title: String,
    status: String,
}

/// Tryby czatu, które Jarvis może przełączać.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatMode {
    Slow,
    EmoteOnly,
    FollowersOnly,
    SubscribersOnly,
}

impl ChatMode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "slow" => Self::Slow,
            "emote_only" => Self::EmoteOnly,
            "followers_only" => Self::FollowersOnly,
            "subscribers_only" => Self::SubscribersOnly,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Slow => "Slow mode",
            Self::EmoteOnly => "Tylko emotki",
            Self::FollowersOnly => "Tylko followersi",
            Self::SubscribersOnly => "Tylko suby",
        }
    }
}

impl Helix {
    /// Szuka kategorii; preferuje dokładne dopasowanie nazwy.
    pub async fn find_category(&self, name: &str) -> Result<(Option<Category>, Vec<String>), HelixError> {
        let resp: Data<Category> = self
            .request(Method::GET, "/search/categories", &[("query", name), ("first", "10")], None)
            .await?
            .json()
            .await?;
        let wanted = normalize(name);
        let exact = resp.data.iter().find(|c| normalize(&c.name) == wanted).cloned();
        let suggestions = resp.data.iter().take(5).map(|c| c.name.clone()).collect();
        Ok((exact.or_else(|| resp.data.first().cloned()), suggestions))
    }

    pub async fn channel_info(&self, broadcaster_id: &str) -> Result<Option<ChannelInfo>, HelixError> {
        let resp: Data<ChannelInfo> = self
            .request(Method::GET, "/channels", &[("broadcaster_id", broadcaster_id)], None)
            .await?
            .json()
            .await?;
        Ok(resp.data.into_iter().next())
    }

    pub async fn modify_channel(&self, broadcaster_id: &str, game_id: Option<&str>, title: Option<&str>) -> Result<(), HelixError> {
        let mut body = json!({});
        if let Some(id) = game_id {
            body["game_id"] = json!(id);
        }
        if let Some(title) = title {
            body["title"] = json!(title.chars().take(140).collect::<String>());
        }
        self.request(Method::PATCH, "/channels", &[("broadcaster_id", broadcaster_id)], Some(&body)).await?;
        Ok(())
    }

    /// `None` = kanał nie nadaje.
    pub async fn live_stream(&self, user_id: &str) -> Result<Option<StreamInfo>, HelixError> {
        let resp: Data<StreamInfo> =
            self.request(Method::GET, "/streams", &[("user_id", user_id)], None).await?.json().await?;
        Ok(resp.data.into_iter().next())
    }

    /// Liczba obserwujących (dostępna bez dodatkowych uprawnień).
    pub async fn follower_total(&self, broadcaster_id: &str) -> Result<u64, HelixError> {
        let resp: Followers = self
            .request(Method::GET, "/channels/followers", &[("broadcaster_id", broadcaster_id), ("first", "1")], None)
            .await?
            .json()
            .await?;
        Ok(resp.total)
    }

    pub async fn user_by_login(&self, login: &str) -> Result<Option<TwitchUser>, HelixError> {
        let login = login.trim().trim_start_matches('@').to_lowercase();
        let resp: Data<TwitchUser> =
            self.request(Method::GET, "/users", &[("login", &login)], None).await?.json().await?;
        Ok(resp.data.into_iter().next())
    }

    pub async fn set_chat_mode(&self, broadcaster_id: &str, mode: ChatMode, enabled: bool, seconds: Option<u32>) -> Result<(), HelixError> {
        let me = self.me().await?;
        let body = match mode {
            ChatMode::Slow => {
                let mut b = json!({ "slow_mode": enabled });
                if enabled {
                    b["slow_mode_wait_time"] = json!(seconds.unwrap_or(30).clamp(3, 120));
                }
                b
            }
            ChatMode::EmoteOnly => json!({ "emote_mode": enabled }),
            ChatMode::FollowersOnly => {
                let mut b = json!({ "follower_mode": enabled });
                if enabled {
                    // Twitch przyjmuje minuty (0–129600); 0 = wystarczy obserwować.
                    b["follower_mode_duration"] = json!((seconds.unwrap_or(0) / 60).min(129_600));
                }
                b
            }
            ChatMode::SubscribersOnly => json!({ "subscriber_mode": enabled }),
        };
        self.request(
            Method::PATCH,
            "/chat/settings",
            &[("broadcaster_id", broadcaster_id), ("moderator_id", &me)],
            Some(&body),
        )
        .await?;
        Ok(())
    }

    /// Czyści cały czat (bez `message_id`).
    pub async fn clear_chat(&self, broadcaster_id: &str) -> Result<(), HelixError> {
        let me = self.me().await?;
        self.request(Method::DELETE, "/moderation/chat", &[("broadcaster_id", broadcaster_id), ("moderator_id", &me)], None)
            .await?;
        Ok(())
    }

    pub async fn announce(&self, broadcaster_id: &str, message: &str) -> Result<(), HelixError> {
        let me = self.me().await?;
        let body = json!({ "message": message.chars().take(500).collect::<String>(), "color": "purple" });
        self.request(
            Method::POST,
            "/chat/announcements",
            &[("broadcaster_id", broadcaster_id), ("moderator_id", &me)],
            Some(&body),
        )
        .await?;
        Ok(())
    }

    pub async fn shoutout(&self, from_id: &str, to_id: &str) -> Result<(), HelixError> {
        let me = self.me().await?;
        self.request(
            Method::POST,
            "/chat/shoutouts",
            &[("from_broadcaster_id", from_id), ("to_broadcaster_id", to_id), ("moderator_id", &me)],
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn start_raid(&self, from_id: &str, to_id: &str) -> Result<(), HelixError> {
        self.request(Method::POST, "/raids", &[("from_broadcaster_id", from_id), ("to_broadcaster_id", to_id)], None)
            .await?;
        Ok(())
    }

    pub async fn cancel_raid(&self, broadcaster_id: &str) -> Result<(), HelixError> {
        self.request(Method::DELETE, "/raids", &[("broadcaster_id", broadcaster_id)], None).await?;
        Ok(())
    }

    /// Zwraca adres klipu.
    pub async fn create_clip(&self, broadcaster_id: &str) -> Result<String, HelixError> {
        let resp: Data<Clip> =
            self.request(Method::POST, "/clips", &[("broadcaster_id", broadcaster_id)], None).await?.json().await?;
        let clip = resp.data.into_iter().next().ok_or_else(|| HelixError::Api {
            status: 200,
            message: "Twitch nie zwrócił klipu".into(),
        })?;
        Ok(format!("https://clips.twitch.tv/{}", clip.id))
    }

    pub async fn create_marker(&self, user_id: &str, description: Option<&str>) -> Result<(), HelixError> {
        let mut body = json!({ "user_id": user_id });
        if let Some(d) = description.filter(|d| !d.trim().is_empty()) {
            body["description"] = json!(d.chars().take(140).collect::<String>());
        }
        self.request(Method::POST, "/streams/markers", &[], Some(&body)).await?;
        Ok(())
    }

    /// Zwraca (długość, sekundy do kolejnej możliwej reklamy).
    pub async fn run_commercial(&self, broadcaster_id: &str, seconds: u32) -> Result<(u32, u32), HelixError> {
        // Twitch akceptuje 30–180 s; zaokrąglamy do pełnych 30 s.
        let length = ((seconds.clamp(30, 180) + 15) / 30) * 30;
        let body = json!({ "broadcaster_id": broadcaster_id, "length": length });
        let resp: Data<Commercial> =
            self.request(Method::POST, "/channels/commercial", &[], Some(&body)).await?.json().await?;
        let c = resp.data.into_iter().next();
        Ok(c.map_or((length, 0), |c| (c.length, c.retry_after)))
    }

    pub async fn create_poll(&self, broadcaster_id: &str, title: &str, choices: &[String], seconds: u32) -> Result<(), HelixError> {
        let choices: Vec<Value> = choices
            .iter()
            .take(5)
            .map(|c| json!({ "title": c.chars().take(25).collect::<String>() }))
            .collect();
        let body = json!({
            "broadcaster_id": broadcaster_id,
            "title": title.chars().take(60).collect::<String>(),
            "choices": choices,
            "duration": seconds.clamp(15, 1800),
        });
        self.request(Method::POST, "/polls", &[], Some(&body)).await?;
        Ok(())
    }

    /// Kończy trwającą ankietę (wyniki zostają widoczne). Zwraca jej tytuł.
    pub async fn end_poll(&self, broadcaster_id: &str) -> Result<Option<String>, HelixError> {
        let resp: Data<Poll> = self
            .request(Method::GET, "/polls", &[("broadcaster_id", broadcaster_id), ("first", "5")], None)
            .await?
            .json()
            .await?;
        let Some(poll) = resp.data.into_iter().find(|p| p.status == "ACTIVE") else { return Ok(None) };
        let body = json!({ "broadcaster_id": broadcaster_id, "id": poll.id, "status": "TERMINATED" });
        self.request(Method::PATCH, "/polls", &[], Some(&body)).await?;
        Ok(Some(poll.title))
    }
}

impl Helix {
    pub async fn add_moderator(&self, broadcaster_id: &str, user_id: &str) -> Result<(), HelixError> {
        self.request(Method::POST, "/moderation/moderators", &[("broadcaster_id", broadcaster_id), ("user_id", user_id)], None)
            .await?;
        Ok(())
    }

    pub async fn remove_moderator(&self, broadcaster_id: &str, user_id: &str) -> Result<(), HelixError> {
        self.request(Method::DELETE, "/moderation/moderators", &[("broadcaster_id", broadcaster_id), ("user_id", user_id)], None)
            .await?;
        Ok(())
    }

    pub async fn add_vip(&self, broadcaster_id: &str, user_id: &str) -> Result<(), HelixError> {
        self.request(Method::POST, "/channels/vips", &[("broadcaster_id", broadcaster_id), ("user_id", user_id)], None).await?;
        Ok(())
    }

    pub async fn remove_vip(&self, broadcaster_id: &str, user_id: &str) -> Result<(), HelixError> {
        self.request(Method::DELETE, "/channels/vips", &[("broadcaster_id", broadcaster_id), ("user_id", user_id)], None).await?;
        Ok(())
    }

    /// Ostrzeżenie: użytkownik nie może pisać, dopóki go nie potwierdzi.
    pub async fn warn_user(&self, broadcaster_id: &str, user_id: &str, reason: &str) -> Result<(), HelixError> {
        let me = self.me().await?;
        let reason: String = reason.chars().take(500).collect();
        let body = json!({ "data": { "user_id": user_id, "reason": reason } });
        self.request(
            Method::POST,
            "/moderation/warnings",
            &[("broadcaster_id", broadcaster_id), ("moderator_id", &me)],
            Some(&body),
        )
        .await?;
        Ok(())
    }

    /// Tryb tarczy: zestaw zabezpieczeń przed rajdami nienawiści.
    pub async fn set_shield_mode(&self, broadcaster_id: &str, active: bool) -> Result<(), HelixError> {
        let me = self.me().await?;
        self.request(
            Method::PUT,
            "/moderation/shield_mode",
            &[("broadcaster_id", broadcaster_id), ("moderator_id", &me)],
            Some(&json!({ "is_active": active })),
        )
        .await?;
        Ok(())
    }

    /// `color`: nazwa z `CHAT_COLORS` albo `#RRGGBB` (Prime/Turbo).
    pub async fn set_chat_color(&self, color: &str) -> Result<(), HelixError> {
        let me = self.me().await?;
        self.request(Method::PUT, "/chat/color", &[("user_id", &me), ("color", color)], None).await?;
        Ok(())
    }

    pub async fn create_prediction(&self, broadcaster_id: &str, title: &str, outcomes: &[String], seconds: u32) -> Result<(), HelixError> {
        let outcomes: Vec<Value> = outcomes
            .iter()
            .take(10)
            .map(|o| json!({ "title": o.chars().take(25).collect::<String>() }))
            .collect();
        let body = json!({
            "broadcaster_id": broadcaster_id,
            "title": title.chars().take(45).collect::<String>(),
            "outcomes": outcomes,
            "prediction_window": seconds.clamp(30, 1800),
        });
        self.request(Method::POST, "/predictions", &[], Some(&body)).await?;
        Ok(())
    }

    /// Kończy trwającą (lub zablokowaną) predykcję. Przy rozstrzygnięciu
    /// `winner` to nazwa zwycięskiej odpowiedzi (dopasowywana luźno).
    /// Zwraca (tytuł predykcji, nazwa zwycięzcy).
    pub async fn end_prediction(&self, broadcaster_id: &str, how: PredictionEnd, winner: Option<&str>) -> Result<(String, Option<String>), HelixError> {
        let resp: Data<Prediction> = self
            .request(Method::GET, "/predictions", &[("broadcaster_id", broadcaster_id), ("first", "5")], None)
            .await?
            .json()
            .await?;
        let Some(p) = resp.data.into_iter().find(|p| p.status == "ACTIVE" || p.status == "LOCKED") else {
            return Err(HelixError::Api { status: 404, message: "Nie ma trwającej predykcji".into() });
        };
        let mut body = json!({ "broadcaster_id": broadcaster_id, "id": p.id });
        let mut winner_name = None;
        match how {
            PredictionEnd::Lock => body["status"] = json!("LOCKED"),
            PredictionEnd::Cancel => body["status"] = json!("CANCELED"),
            PredictionEnd::Resolve => {
                let wanted = normalize(winner.unwrap_or_default());
                let outcome = p
                    .outcomes
                    .iter()
                    .find(|o| normalize(&o.title) == wanted)
                    .or_else(|| p.outcomes.iter().find(|o| !wanted.is_empty() && (normalize(&o.title).contains(&wanted) || wanted.contains(&normalize(&o.title)))))
                    .ok_or_else(|| HelixError::Api {
                        status: 400,
                        message: format!(
                            "Nie wiem, która odpowiedź wygrała. Możliwe: {}",
                            p.outcomes.iter().map(|o| o.title.as_str()).collect::<Vec<_>>().join(", ")
                        ),
                    })?;
                body["status"] = json!("RESOLVED");
                body["winning_outcome_id"] = json!(outcome.id);
                winner_name = Some(outcome.title.clone());
            }
        }
        self.request(Method::PATCH, "/predictions", &[], Some(&body)).await?;
        Ok((p.title, winner_name))
    }
}

/// Porównanie nazw bez wielkości liter, spacji i znaków interpunkcyjnych.
pub fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_names_for_matching() {
        assert_eq!(normalize("Grand Theft Auto IV"), normalize("grand theft auto iv"));
        assert_eq!(normalize("Just Chatting"), "justchatting");
        assert_eq!(normalize("@Mod_Ziomek"), "modziomek");
    }

    #[test]
    fn chat_modes_parse() {
        assert_eq!(ChatMode::parse("emote_only"), Some(ChatMode::EmoteOnly));
        assert_eq!(ChatMode::parse("cokolwiek"), None);
    }
}
