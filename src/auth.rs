//! Logowanie przez Twitch OAuth (Device Code Grant Flow) i zarządzanie tokenem.
//!
//! ## Dlaczego Device Code Flow
//! Aplikacja desktopowa nie potrafi bezpiecznie ukryć `client_secret`
//! (każdy może go wyciągnąć z pliku binarnego). Device Code Flow jest
//! przeznaczony dla takich „publicznych klientów”:
//! 1. prosimy Twitcha o kod urządzenia (`POST /oauth2/device`),
//! 2. użytkownik otwiera `verification_uri` w przeglądarce i zatwierdza dostęp,
//! 3. w tym czasie odpytujemy `POST /oauth2/token` aż do sukcesu.
//!
//! Nie potrzebujemy sekretu ani lokalnego serwera HTTP na `localhost`.
//!
//! ## Cykl życia tokena
//! * access token żyje ~4 h, odświeżamy go 5 min przed wygaśnięciem,
//! * refresh token klienta publicznego jest JEDNORAZOWY — dlatego
//!   odświeżanie jest serializowane muteksem (dwa równoległe odświeżenia
//!   unieważniłyby sesję),
//! * Twitch wymaga walidacji tokena przy starcie i co godzinę,
//! * przy wylogowaniu token jest unieważniany po stronie Twitcha.
//!
//! ## Przechowywanie
//! Token trafia wyłącznie do systemowego sejfu (`keyring`): Windows
//! Credential Manager, macOS Keychain lub Linux Secret Service. Nigdy do
//! pliku ustawień, logów ani komunikatów błędów.

use std::{fmt, time::Duration};

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::{sync::Mutex, time::Instant};

/// Client ID aplikacji zarejestrowanej na dev.twitch.tv (typ: Public).
/// To NIE jest sekret — jest widoczny w każdym adresie autoryzacji.
/// Można go nadpisać zmienną środowiskową `TWITCH_CLIENT_ID`.
pub const DEFAULT_CLIENT_ID: &str = "d4n4e7qloc0ft7qix4sihnawfq2ohn";

/// Minimalny zestaw uprawnień potrzebny aplikacji.
pub const SCOPES: &[&str] = &[
    "moderator:manage:banned_users",  // ban, timeout, unban
    "moderator:manage:chat_messages", // usuwanie wiadomości
    "user:read:moderated_channels",   // sprawdzenie, czy jesteśmy modem kanału
    "user:write:chat",                // wysyłanie wiadomości na czat
];

const ID_BASE: &str = "https://id.twitch.tv/oauth2";
const KEYRING_SERVICE: &str = "limbochat";
const KEYRING_USER: &str = "twitch-oauth-token";
const REFRESH_MARGIN_SECS: i64 = 5 * 60;

/// Nazwa wpisu w sejfie. Wtyczka OBS używa osobnego wpisu: refresh tokeny
/// są jednorazowe, więc aplikacja PC i OBS działające naraz nie mogą
/// dzielić jednej sesji (jedna unieważniałaby drugą przy odświeżaniu).
static KEYRING_ACCOUNT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

#[allow(dead_code)] // używane tylko przez wtyczkę OBS
pub fn set_keyring_account(name: &'static str) {
    let _ = KEYRING_ACCOUNT.set(name);
}

/// Dodatkowe uprawnienia (wtyczka OBS prosi o więcej — dla Jarvisa).
static EXTRA_SCOPES: std::sync::OnceLock<&'static [&'static str]> = std::sync::OnceLock::new();

#[allow(dead_code)] // używane tylko przez wtyczkę OBS
pub fn set_extra_scopes(scopes: &'static [&'static str]) {
    let _ = EXTRA_SCOPES.set(scopes);
}

/// Wszystkie uprawnienia, o które prosi ten program.
pub fn scopes() -> Vec<&'static str> {
    SCOPES.iter().chain(EXTRA_SCOPES.get().copied().unwrap_or_default()).copied().collect()
}

fn keyring_account() -> &'static str {
    KEYRING_ACCOUNT.get().copied().unwrap_or(KEYRING_USER)
}

pub fn client_id() -> String {
    std::env::var("TWITCH_CLIENT_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_owned())
}

// ── Modele ─────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub user_id: String,
    pub login: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// Ręczny `Debug` — token nie może przypadkiem wyciec do logów.
impl fmt::Debug for StoredToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredToken")
            .field("login", &self.login)
            .field("user_id", &self.user_id)
            .field("expires_at", &self.expires_at)
            .field("access_token", &"<ukryty>")
            .field("refresh_token", &"<ukryty>")
            .finish()
    }
}

#[derive(Clone, Deserialize)]
pub struct DeviceCode {
    device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    interval: u64,
}

#[derive(Debug, Clone)]
pub struct UserInfo {
    pub login: String,
    pub user_id: String,
}

impl From<&StoredToken> for UserInfo {
    fn from(t: &StoredToken) -> Self {
        Self { login: t.login.clone(), user_id: t.user_id.clone() }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    #[serde(default)]
    scope: Vec<String>,
}

#[derive(Deserialize)]
struct ValidateResponse {
    login: String,
    user_id: String,
}

#[derive(Deserialize, Default)]
struct OAuthError {
    #[serde(default)]
    message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("nie jesteś zalogowany")]
    NotLoggedIn,
    /// Twitch odrzucił refresh token — wymagane ponowne logowanie.
    #[error("sesja wygasła — zaloguj się ponownie")]
    SessionExpired,
    /// Błąd sieci lub chwilowy problem — sesja pozostaje zachowana.
    #[error(transparent)]
    Transient(#[from] anyhow::Error),
}

// ── Klient endpointów id.twitch.tv ────────────────────────────────────

pub struct AuthClient {
    http: reqwest::Client,
    client_id: String,
}

impl AuthClient {
    pub fn new(http: reqwest::Client, client_id: String) -> Self {
        Self { http, client_id }
    }

    pub async fn start_device_flow(&self) -> anyhow::Result<DeviceCode> {
        let scopes = scopes().join(" ");
        let resp = self
            .http
            .post(format!("{ID_BASE}/device"))
            .form(&[("client_id", self.client_id.as_str()), ("scopes", scopes.as_str())])
            .send()
            .await
            .context("nie można połączyć się z id.twitch.tv")?;

        if !resp.status().is_success() {
            let err = resp.json::<OAuthError>().await.unwrap_or_default();
            bail!("Twitch odrzucił żądanie logowania: {} (sprawdź Client ID i typ aplikacji „Public”)", err.message);
        }
        resp.json().await.context("niepoprawna odpowiedź /oauth2/device")
    }

    /// Odpytuje Twitcha, aż użytkownik zatwierdzi dostęp albo kod wygaśnie.
    pub async fn poll_device_token(&self, dc: &DeviceCode) -> anyhow::Result<StoredToken> {
        let deadline = Instant::now() + Duration::from_secs(dc.expires_in);
        let mut interval = Duration::from_secs(dc.interval.max(1));
        let scopes = scopes().join(" ");

        loop {
            tokio::time::sleep(interval).await;
            if Instant::now() >= deadline {
                bail!("kod logowania wygasł — spróbuj ponownie");
            }

            let resp = self
                .http
                .post(format!("{ID_BASE}/token"))
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("scopes", scopes.as_str()),
                    ("device_code", dc.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                // Chwilowy błąd sieci w trakcie czekania — próbujemy dalej.
                Err(e) => {
                    tracing::warn!("błąd odpytywania o token: {e}");
                    continue;
                }
            };

            if resp.status().is_success() {
                let token: TokenResponse = resp.json().await.context("niepoprawna odpowiedź /oauth2/token")?;
                return self.complete(token).await;
            }

            let err = resp.json::<OAuthError>().await.unwrap_or_default();
            match err.message.as_str() {
                "authorization_pending" => continue,
                "slow_down" => interval += Duration::from_secs(5),
                m if m.contains("invalid device code") => bail!("kod logowania wygasł — spróbuj ponownie"),
                m => bail!("Twitch odrzucił logowanie: {m}"),
            }
        }
    }

    /// Uzupełnia odpowiedź tokenową o login i ID użytkownika (z /validate).
    async fn complete(&self, t: TokenResponse) -> anyhow::Result<StoredToken> {
        let v = self
            .validate(&t.access_token)
            .await?
            .ok_or_else(|| anyhow!("Twitch odrzucił świeżo wydany token"))?;
        Ok(StoredToken {
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            expires_at: Utc::now() + chrono::Duration::seconds(t.expires_in),
            user_id: v.user_id,
            login: v.login,
            scopes: t.scope,
        })
    }

    async fn refresh(&self, old: &StoredToken) -> Result<StoredToken, AuthError> {
        let resp = self
            .http
            .post(format!("{ID_BASE}/token"))
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", old.refresh_token.as_str()),
            ])
            .send()
            .await
            .context("błąd sieci przy odświeżaniu tokena")?;

        let status = resp.status();
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(AuthError::SessionExpired);
        }
        let t: TokenResponse = resp
            .error_for_status()
            .context("błąd serwera przy odświeżaniu tokena")?
            .json()
            .await
            .context("niepoprawna odpowiedź przy odświeżaniu")?;

        Ok(StoredToken {
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            expires_at: Utc::now() + chrono::Duration::seconds(t.expires_in),
            user_id: old.user_id.clone(),
            login: old.login.clone(),
            scopes: if t.scope.is_empty() { old.scopes.clone() } else { t.scope },
        })
    }

    /// `Ok(None)` = token nieważny (401). `Err` = problem sieciowy.
    async fn validate(&self, access_token: &str) -> anyhow::Result<Option<ValidateResponse>> {
        let resp = self
            .http
            .get(format!("{ID_BASE}/validate"))
            .header("Authorization", format!("OAuth {access_token}"))
            .send()
            .await
            .context("błąd sieci przy walidacji tokena")?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(None);
        }
        Ok(Some(resp.error_for_status()?.json().await?))
    }

    async fn revoke(&self, access_token: &str) {
        let result = self
            .http
            .post(format!("{ID_BASE}/revoke"))
            .form(&[("client_id", self.client_id.as_str()), ("token", access_token)])
            .send()
            .await;
        if let Err(e) = result {
            tracing::warn!("nie udało się unieważnić tokena po stronie Twitcha: {e}");
        }
    }
}

// ── Systemowy sejf ─────────────────────────────────────────────────────

/// Operacje `keyring` są blokujące (D-Bus, Win32 API), więc zawsze
/// wywołujemy je przez `spawn_blocking`, nigdy bezpośrednio w zadaniu async.
mod vault {
    use super::*;

    fn entry() -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, keyring_account())
    }

    pub fn load() -> anyhow::Result<Option<StoredToken>> {
        match entry()?.get_password() {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(token) => Ok(Some(token)),
                Err(_) => {
                    tracing::warn!("uszkodzony wpis w sejfie — zostanie zignorowany");
                    Ok(None)
                }
            },
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(token: &StoredToken) -> anyhow::Result<()> {
        entry()?.set_password(&serde_json::to_string(token)?)?;
        Ok(())
    }

    pub fn delete() -> anyhow::Result<()> {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(f).await.context("zadanie sejfu zostało przerwane")?
}

// ── Menedżer tokena ────────────────────────────────────────────────────

/// Jedyne źródło prawdy o sesji. Wszystkie odczyty i odświeżenia tokena
/// przechodzą przez muteks, więc refresh token nigdy nie zostanie użyty dwa razy.
pub struct TokenManager {
    auth: AuthClient,
    token: Mutex<Option<StoredToken>>,
}

impl TokenManager {
    pub fn new(auth: AuthClient) -> Self {
        Self { auth, token: Mutex::new(None) }
    }

    pub fn auth(&self) -> &AuthClient {
        &self.auth
    }

    pub async fn user(&self) -> Option<UserInfo> {
        self.token.lock().await.as_ref().map(UserInfo::from)
    }

    /// Uprawnienia wymagane przez tę wersję aplikacji, których zapisany token
    /// nie ma (np. token sprzed dodania wysyłania wiadomości).
    pub async fn missing_scopes(&self) -> Vec<&'static str> {
        let slot = self.token.lock().await;
        let Some(token) = slot.as_ref() else { return Vec::new() };
        scopes().into_iter().filter(|s| !token.scopes.iter().any(|t| t == s)).collect()
    }

    /// Przy starcie: wczytaj token z sejfu i zweryfikuj go.
    pub async fn restore(&self) -> anyhow::Result<Option<UserInfo>> {
        let Some(stored) = blocking(vault::load).await? else { return Ok(None) };
        *self.token.lock().await = Some(stored);
        match self.validate_now().await {
            Ok(user) => Ok(user),
            // Brak sieci przy starcie: zakładamy, że token jest ważny.
            // Ewentualny 401 z Helix i tak wymusi odświeżenie.
            Err(e) => {
                tracing::warn!("nie udało się zweryfikować tokena przy starcie: {e:#}");
                Ok(self.user().await)
            }
        }
    }

    /// Zapisuje nowy token. Zwraca ostrzeżenie, jeśli sejf jest niedostępny
    /// (np. Linux bez gnome-keyring/KWallet) — sesja działa wtedy tylko do zamknięcia.
    pub async fn set(&self, token: StoredToken) -> (UserInfo, Option<String>) {
        let user = UserInfo::from(&token);
        let warning = self.persist(&token).await;
        *self.token.lock().await = Some(token);
        (user, warning)
    }

    async fn persist(&self, token: &StoredToken) -> Option<String> {
        let copy = token.clone();
        match blocking(move || vault::save(&copy)).await {
            Ok(()) => None,
            Err(e) => {
                tracing::warn!("sejf systemowy niedostępny: {e:#}");
                Some("Sejf systemowy jest niedostępny — logowanie będzie ważne tylko do zamknięcia aplikacji.".to_owned())
            }
        }
    }

    /// Ważny access token i ID zalogowanego użytkownika (do `moderator_id`).
    pub async fn access_token(&self) -> Result<(String, String), AuthError> {
        let mut slot = self.token.lock().await;
        let token = slot.as_ref().ok_or(AuthError::NotLoggedIn)?;
        let expiring = token.expires_at - Utc::now() < chrono::Duration::seconds(REFRESH_MARGIN_SECS);
        if expiring {
            self.refresh_locked(&mut slot).await?;
        }
        let token = slot.as_ref().ok_or(AuthError::NotLoggedIn)?;
        Ok((token.access_token.clone(), token.user_id.clone()))
    }

    /// Wywoływane po 401 z Helix. Jeśli inne zadanie już odświeżyło token
    /// (aktualny różni się od `stale`), nie odświeżamy ponownie.
    pub async fn force_refresh(&self, stale: &str) -> Result<(), AuthError> {
        let mut slot = self.token.lock().await;
        match slot.as_ref() {
            None => Err(AuthError::NotLoggedIn),
            Some(t) if t.access_token != stale => Ok(()),
            Some(_) => self.refresh_locked(&mut slot).await,
        }
    }

    async fn refresh_locked(&self, slot: &mut Option<StoredToken>) -> Result<(), AuthError> {
        let old = slot.as_ref().ok_or(AuthError::NotLoggedIn)?;
        match self.auth.refresh(old).await {
            Ok(new) => {
                if let Some(warning) = self.persist(&new).await {
                    tracing::warn!("{warning}");
                }
                *slot = Some(new);
                Ok(())
            }
            Err(AuthError::SessionExpired) => {
                *slot = None;
                let _ = blocking(vault::delete).await;
                Err(AuthError::SessionExpired)
            }
            Err(e) => Err(e),
        }
    }

    /// Walidacja wymagana przez Twitcha (start + co godzinę).
    /// `Ok(None)` oznacza, że sesja wygasła i nie dało się jej odnowić.
    pub async fn validate_now(&self) -> anyhow::Result<Option<UserInfo>> {
        let mut slot = self.token.lock().await;
        let Some(token) = slot.as_ref() else { return Ok(None) };

        if self.auth.validate(&token.access_token).await?.is_some() {
            return Ok(slot.as_ref().map(UserInfo::from));
        }
        match self.refresh_locked(&mut slot).await {
            Ok(()) => Ok(slot.as_ref().map(UserInfo::from)),
            Err(AuthError::SessionExpired | AuthError::NotLoggedIn) => Ok(None),
            Err(AuthError::Transient(e)) => Err(e),
        }
    }

    /// Wylogowanie: unieważnienie tokena u Twitcha i usunięcie z sejfu.
    pub async fn logout(&self) {
        let token = self.token.lock().await.take();
        if let Some(t) = token {
            self.auth.revoke(&t.access_token).await;
        }
        if let Err(e) = blocking(vault::delete).await {
            tracing::warn!("nie udało się usunąć tokena z sejfu: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_tokens() {
        let token = StoredToken {
            access_token: "sekretny-access".into(),
            refresh_token: "sekretny-refresh".into(),
            expires_at: Utc::now(),
            user_id: "123".into(),
            login: "modek".into(),
            scopes: vec![],
        };
        let printed = format!("{token:?}");
        assert!(!printed.contains("sekretny"), "token wyciekł do Debug: {printed}");
        assert!(printed.contains("modek"));
    }

    #[test]
    fn requests_only_user_and_moderation_scopes() {
        assert!(SCOPES.iter().all(|s| s.starts_with("moderator:") || s.starts_with("user:")));
    }
}
