//! Official Kick OAuth 2.1 + public REST API + authenticated webhook ingestion.
//! The tunnel may expose ONLY 127.0.0.1:47251, never the control panel.
#[cfg(test)]
#[path = "kick_tests.rs"]
mod integration;
use crate::{
    emotes::{EmoteScale, EmoteSource, EmoteStore},
    events::{AlertKind, ChatAlert, Command, UiEvent},
    parser::{self, Badge, ParsedMessage, Segment},
    platform,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chrono::Utc;
use rand_core::{OsRng, RngCore};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spki::der::Decode;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc, watch};

pub const WEBHOOK_PORT: u16 = 47251;
/// Scripted local setup: secret arrives on stdin, never as an argument or output.
pub fn cli_configure() -> i32 {
    use std::io::Read;
    #[derive(Deserialize)]
    struct Input {
        client_id: String,
        client_secret: String,
        #[serde(default)]
        webhooks: bool,
    }
    let mut input = String::new();
    if std::io::stdin()
        .take(16385)
        .read_to_string(&mut input)
        .is_err()
        || input.len() > 16384
    {
        eprintln!("Niepoprawne dane konfiguracji Kicka.");
        return 1;
    }
    let Ok(data) = serde_json::from_str::<Input>(&input) else {
        eprintln!("Oczekiwano JSON z client_id i client_secret na stdin.");
        return 1;
    };
    if data.client_id.is_empty()
        || data.client_id.len() > 200
        || !data
            .client_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || data.client_secret.trim().is_empty()
        || data.client_secret.len() > 4096
    {
        eprintln!("Niepoprawne dane aplikacji Kicka.");
        return 1;
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return 1;
    };
    let mut config = crate::config::load();
    let result = rt.block_on(async {
        vault_write("kick-client-secret", Some(data.client_secret.trim().into())).await?;
        if config.kick.client_id != data.client_id {
            vault_write("kick-session", None).await?;
        }
        Ok::<_, String>(())
    });
    if let Err(e) = result {
        eprintln!("{e}");
        return 1;
    }
    config.kick = Config {
        client_id: data.client_id,
        webhooks: data.webhooks,
    };
    crate::config::save(&config);
    if crate::config::load().kick.client_id != config.kick.client_id {
        eprintln!("Nie udało się zapisać ustawień Kicka.");
        return 1;
    }
    println!("Kick: konfiguracja zapisana; sekret jest w sejfie systemowym.");
    0
}
pub const CALLBACK: &str = "http://localhost:47250/kick/callback";
const API: &str = "https://api.kick.com/public";
const ID: &str = "https://id.kick.com/oauth";
const EVENTS: &[&str] = &[
    "chat.message.sent",
    "channel.followed",
    "channel.subscription.new",
    "channel.subscription.renewal",
    "channel.subscription.gifts",
    "channel.reward.redemption.updated",
    "moderation.banned",
    "kicks.gifted",
];
const SCOPES: &str = "user:read channel:read channel:write chat:write moderation:ban moderation:chat_message:manage events:subscribe";

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub client_id: String,
    pub webhooks: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Session {
    access_token: String,
    refresh_token: String,
    expires: i64,
    login: String,
    user_id: u64,
}
struct Pending {
    state: String,
    verifier: String,
    at: Instant,
}
#[derive(Default)]
struct State {
    session: Option<Session>,
    app: Option<(String, Instant)>,
    pending: Option<Pending>,
    channels: HashMap<String, u64>,
    subscriptions: HashMap<String, (String, String)>,
    last_event: Option<i64>,
    error: Option<String>,
    auth_url: Option<String>,
    listener: bool,
    last_status: Option<Instant>,
}
pub struct Client {
    http: reqwest::Client,
    config: Mutex<Config>,
    state: Mutex<State>,
    auth_gate: Mutex<()>,
    subscription_gate: Mutex<()>,
    key: Mutex<Option<(Vec<u8>, Instant)>>,
    seen: Mutex<VecDeque<String>>,
    ui: mpsc::UnboundedSender<UiEvent>,
    store: Option<Arc<EmoteStore>>,
    api: String,
    identity: String,
    demo: bool,
    native: Mutex<HashMap<String, HashMap<String, String>>>,
}

fn random() -> String {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}
fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
fn account(user: &Value) -> String {
    user["channel_slug"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| user["username"].as_str())
        .unwrap_or("anonymous")
        .to_lowercase()
}
fn safe_id(id: &str) -> Result<u64, String> {
    id.trim_start_matches("kick:")
        .parse()
        .map_err(|_| "Niepoprawne ID Kicka.".into())
}

async fn vault_read(name: &'static str) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        keyring::Entry::new("limbochat", name)
            .ok()?
            .get_password()
            .ok()
    })
    .await
    .ok()
    .flatten()
}
async fn vault_write(name: &'static str, text: Option<String>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let entry =
            keyring::Entry::new("limbochat", name).map_err(|_| "Sejf Kicka jest niedostępny.")?;
        match text {
            Some(text) => entry
                .set_password(&text)
                .map_err(|_| "Nie udało się zapisać danych Kicka w sejfie."),
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err("Nie udało się usunąć danych Kicka z sejfu."),
            },
        }
    })
    .await
    .map_err(|_| "Błąd sejfu Kicka.".to_owned())?
    .map_err(str::to_owned)
}

impl Client {
    pub fn new(
        config: Config,
        ui: mpsc::UnboundedSender<UiEvent>,
        store: Option<Arc<EmoteStore>>,
        demo: bool,
    ) -> Arc<Self> {
        let state = if demo {
            State {
                session: Some(Session {
                    access_token: String::new(),
                    refresh_token: String::new(),
                    expires: i64::MAX,
                    login: "ty".into(),
                    user_id: 0,
                }),
                ..State::default()
            }
        } else {
            State::default()
        };
        Arc::new(Self {
            http: crate::backend::build_http_client(),
            config: Mutex::new(config),
            state: Mutex::new(state),
            auth_gate: Mutex::new(()),
            subscription_gate: Mutex::new(()),
            key: Mutex::new(None),
            seen: Mutex::new(VecDeque::new()),
            ui,
            store,
            api: API.into(),
            identity: ID.into(),
            demo,
            native: Mutex::new(HashMap::new()),
        })
    }
    pub fn start(self: &Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let this = self.clone();
        tokio::spawn(async move {
            if !this.demo {
                let _auth = this.auth_gate.lock().await;
                let session = vault_read("kick-session")
                    .await
                    .and_then(|s| serde_json::from_str(&s).ok());
                this.state.lock().await.session = session;
            }
            this.refresh_roles().await;
            this.emit().await;
            let listener =
                match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, WEBHOOK_PORT))
                    .await
                {
                    Ok(l) => l,
                    Err(_) => {
                        this.error("Port webhooka Kicka 47251 jest zajęty.").await;
                        return;
                    }
                };
            this.state.lock().await.listener = true;
            this.emit().await;
            let app = axum::Router::new()
                .route("/kick/events", axum::routing::post(webhook))
                .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
                .with_state(this.clone());
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown.changed().await;
                })
                .await;
        });
    }
    pub async fn status(&self) -> Value {
        let secret = if self.demo {
            false
        } else {
            vault_read("kick-client-secret").await.is_some()
        };
        let s = self.state.lock().await;
        json!({"state":if s.pending.is_some(){"pending"}else if s.session.is_some(){"logged_in"}else{"logged_out"},
            "login":s.session.as_ref().map(|s|&s.login),"user_id":s.session.as_ref().map(|s|s.user_id.to_string()),
            "secret_saved":secret,"auth_url":s.auth_url,"listener":s.listener,"last_event":s.last_event,"error":s.error,
            "channels":s.channels.keys().collect::<Vec<_>>(),"subscriptions":s.subscriptions.len(),"callback":CALLBACK,
            "webhook_local":format!("http://127.0.0.1:{WEBHOOK_PORT}/kick/events")})
    }
    pub async fn emit(&self) {
        let _ = self.ui.send(UiEvent::PlatformStatus {
            platform: "kick".into(),
            data: self.status().await,
        });
    }
    pub(crate) async fn error(&self, message: &str) {
        self.state.lock().await.error = Some(message.into());
        self.emit().await;
    }
    pub async fn configure(
        &self,
        config: Config,
        secret: Option<String>,
        clear: bool,
    ) -> Result<(), String> {
        if self.demo {
            return Err("W trybie demo konfiguracja Kicka nie jest zapisywana.".into());
        }
        let _subscription = self.subscription_gate.lock().await;
        let channels = self
            .state
            .lock()
            .await
            .channels
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for channel in channels {
            let _ = self.ui.send(UiEvent::Disconnected { channel });
        }
        if clear {
            self.logout().await?;
            vault_write("kick-client-secret", None).await?;
        } else if let Some(secret) = secret.filter(|s| !s.trim().is_empty()) {
            if secret.len() > 4096 {
                return Err("Sekret Kicka jest zbyt długi.".into());
            }
            vault_write("kick-client-secret", Some(secret.trim().into())).await?;
        }
        let changed = self.config.lock().await.client_id != config.client_id;
        if changed {
            self.logout().await?;
        }
        *self.config.lock().await = config;
        let mut s = self.state.lock().await;
        s.app = None;
        s.error = None;
        s.subscriptions.clear();
        drop(s);
        self.emit().await;
        Ok(())
    }
    pub async fn begin_login(&self) -> Result<String, String> {
        if self.demo {
            return Err("Tryb demo używa konta testowego Kicka.".into());
        }
        let config = self.config.lock().await.clone();
        if config.client_id.is_empty() || vault_read("kick-client-secret").await.is_none() {
            return Err("Uzupełnij Client ID i Client Secret Kicka w ustawieniach.".into());
        }
        // Validate the app before opening the browser. This distinguishes invalid
        // credentials from a redirect URI mismatch in the developer dashboard.
        self.cancel_login().await;
        if let Err(error) = self.access(true, None).await {
            self.error(&error).await;
            return Err(error);
        }
        let state = random();
        let verifier = random();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = reqwest::Url::parse(&format!("{}/authorize", self.identity))
            .map_err(|_| "Niepoprawny adres OAuth.")?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", config.client_id.as_str()),
            ("redirect_uri", CALLBACK),
            ("scope", SCOPES),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
        ]);
        let url = url.to_string();
        let mut s = self.state.lock().await;
        s.pending = Some(Pending {
            state,
            verifier,
            at: Instant::now(),
        });
        s.auth_url = Some(url.clone());
        s.error = None;
        drop(s);
        self.emit().await;
        Ok(url)
    }
    pub async fn cancel_login(&self) {
        let mut s = self.state.lock().await;
        s.pending = None;
        s.auth_url = None;
        drop(s);
        self.emit().await;
    }
    pub async fn callback(&self, code: &str, state: &str) -> Result<(), String> {
        let _gate = self.auth_gate.lock().await;
        let mut s = self.state.lock().await;
        let pending = s
            .pending
            .as_ref()
            .ok_or("Brak rozpoczętego logowania Kicka.")?;
        if pending.state != state || pending.at.elapsed() > Duration::from_secs(600) {
            return Err("Nieprawidłowy lub wygasły stan logowania Kicka.".into());
        }
        let pending = s.pending.take().unwrap();
        s.auth_url = None;
        drop(s);
        let mut session = self
            .token(vec![
                ("grant_type", "authorization_code".into()),
                ("code", code.into()),
                ("code_verifier", pending.verifier),
                ("redirect_uri", CALLBACK.into()),
            ])
            .await?;
        let v = self
            .raw(Method::GET, "v1/users", None, &session.access_token)
            .await?;
        let user = v["data"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or("Kick nie zwrócił konta.")?;
        session.user_id = number(&user["user_id"]).ok_or("Kick nie zwrócił ID konta.")?;
        let channel = self
            .raw(Method::GET, "v1/channels", None, &session.access_token)
            .await?;
        session.login = channel["data"][0]["slug"]
            .as_str()
            .ok_or("Kick nie zwrócił nazwy kanału.")?
            .into();
        self.persist(&session).await;
        self.state.lock().await.session = Some(session);
        self.emit().await;
        self.refresh_roles().await;
        Ok(())
    }
    async fn token(&self, mut fields: Vec<(&str, String)>) -> Result<Session, String> {
        let config = self.config.lock().await.clone();
        let secret = vault_read("kick-client-secret")
            .await
            .ok_or("Brak Client Secret Kicka.")?;
        fields.extend([("client_id", config.client_id), ("client_secret", secret)]);
        let response = self
            .http
            .post(format!("{}/token", self.identity))
            .form(&fields)
            .send()
            .await
            .map_err(|_| "Nie można połączyć się z OAuth Kicka.")?;
        if !response.status().is_success() {
            return Err(format!(
                "Kick OAuth: HTTP {}. Sprawdź dane aplikacji i zaloguj się ponownie.",
                response.status().as_u16()
            ));
        }
        let v: Value = response
            .json()
            .await
            .map_err(|_| "Błędna odpowiedź OAuth Kicka.")?;
        let access_token = v["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Kick nie zwrócił tokena.")?
            .into();
        Ok(Session {
            access_token,
            refresh_token: v["refresh_token"].as_str().unwrap_or_default().into(),
            expires: Utc::now().timestamp() + number(&v["expires_in"]).unwrap_or(3600) as i64,
            login: String::new(),
            user_id: 0,
        })
    }
    async fn persist(&self, session: &Session) {
        if let Err(text) = vault_write("kick-session", serde_json::to_string(session).ok()).await {
            let _ = self.ui.send(UiEvent::error(format!(
                "{text} Sesja Kicka działa do zamknięcia programu."
            )));
        }
    }
    async fn access(&self, app: bool, stale: Option<&str>) -> Result<String, String> {
        let _gate = self.auth_gate.lock().await;
        if app {
            if let Some((token, expires)) = &self.state.lock().await.app {
                if *expires > Instant::now() && stale != Some(token.as_str()) {
                    return Ok(token.clone());
                }
            }
            let token = self
                .token(vec![("grant_type", "client_credentials".into())])
                .await?;
            let seconds = (token.expires - Utc::now().timestamp() - 60).max(1) as u64;
            self.state.lock().await.app = Some((
                token.access_token.clone(),
                Instant::now() + Duration::from_secs(seconds),
            ));
            return Ok(token.access_token);
        }
        let old = self
            .state
            .lock()
            .await
            .session
            .clone()
            .ok_or("Połącz konto Kick w ustawieniach.")?;
        if old.expires > Utc::now().timestamp() + 60 && stale != Some(old.access_token.as_str()) {
            return Ok(old.access_token);
        }
        let mut new = self
            .token(vec![
                ("grant_type", "refresh_token".into()),
                ("refresh_token", old.refresh_token.clone()),
            ])
            .await?;
        new.login = old.login;
        new.user_id = old.user_id;
        if new.refresh_token.is_empty() {
            new.refresh_token = old.refresh_token;
        }
        self.persist(&new).await;
        self.state.lock().await.session = Some(new.clone());
        Ok(new.access_token)
    }
    pub async fn logout(&self) -> Result<(), String> {
        if self.demo {
            self.state.lock().await.session = None;
            self.emit().await;
            return Ok(());
        }
        let _gate = self.auth_gate.lock().await;
        let session = self.state.lock().await.session.take();
        if let Some(s) = session {
            let _ = self
                .http
                .post(format!("{}/revoke", self.identity))
                .query(&[
                    ("token", s.access_token),
                    ("token_hint_type", "access_token".into()),
                ])
                .send()
                .await;
        }
        vault_write("kick-session", None).await?;
        self.cancel_login().await;
        self.refresh_roles().await;
        Ok(())
    }
    async fn raw(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        token: &str,
    ) -> Result<Value, String> {
        let mut request = self
            .http
            .request(method, format!("{}/{path}", self.api))
            .bearer_auth(token);
        if let Some(body) = body {
            request = request.json(body)
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Błąd sieci Kicka. Spróbuj ponownie.".to_owned())?;
        let status = response.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 => "KICK_401".into(),
                403 => "Kick: brak uprawnień do tej akcji lub kanału.".into(),
                429 => "Kick: limit zapytań. Poczekaj przed ponowieniem.".into(),
                _ => format!("Kick: API zwróciło HTTP {}.", status.as_u16()),
            });
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(json!({"ok":true}));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| "Nie udało się odczytać odpowiedzi Kicka.")?;
        if bytes.is_empty() {
            Ok(json!({"ok":true}))
        } else {
            serde_json::from_slice(&bytes).map_err(|_| "Błędna odpowiedź JSON Kicka.".into())
        }
    }
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        app: bool,
    ) -> Result<Value, String> {
        let token = self.access(app, None).await?;
        match self.raw(method.clone(), path, body.as_ref(), &token).await {
            Err(e) if e == "KICK_401" => {
                let fresh = self.access(app, Some(&token)).await?;
                self.raw(method, path, body.as_ref(), &fresh)
                    .await
                    .map_err(|e| {
                        if e == "KICK_401" {
                            "Sesja Kicka wygasła. Połącz konto ponownie.".into()
                        } else {
                            e
                        }
                    })
            }
            r => r,
        }
    }
    pub async fn refresh_roles(&self) {
        let s = self.state.lock().await;
        for (channel, id) in &s.channels {
            let can = s.session.as_ref().is_some_and(|s| s.user_id == *id);
            let _ = self.ui.send(UiEvent::ModStatus {
                channel: channel.clone(),
                can_moderate: can,
            });
        }
    }
    pub async fn join(self: &Arc<Self>, channel: String) -> Result<(), String> {
        if self.demo {
            return Ok(());
        }
        let _gate = self.subscription_gate.lock().await;
        let slug = platform::slug(&channel);
        let v = self
            .request(Method::GET, &format!("v1/channels?slug={slug}"), None, true)
            .await?;
        let data = v["data"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or("Nie znaleziono kanału Kicka.")?;
        let id = number(&data["broadcaster_user_id"]).ok_or("Kick nie zwrócił ID kanału.")?;
        self.state.lock().await.channels.insert(channel.clone(), id);
        if !self.config.lock().await.webhooks {
            return Err(
                "Włącz webhooki Kicka w ustawieniach i skonfiguruj publiczny adres odbioru.".into(),
            );
        }
        // App tokens are required here: user tokens silently substitute the logged-in broadcaster.
        let old = self
            .request(
                Method::GET,
                &format!("v1/events/subscriptions?broadcaster_user_id={id}"),
                None,
                true,
            )
            .await?;
        let mut subscribed = HashMap::new();
        for v in old["data"].as_array().into_iter().flatten() {
            if number(&v["broadcaster_user_id"]) != Some(id) {
                continue;
            }
            if let (Some(name), Some(sid)) = (
                v["event"].as_str().or_else(|| v["name"].as_str()),
                v["id"].as_str().or_else(|| v["subscription_id"].as_str()),
            ) {
                if EVENTS.contains(&name) {
                    subscribed.insert(name.to_owned(), sid.to_owned());
                }
            }
        }
        let missing: Vec<Value> = EVENTS
            .iter()
            .filter(|name| !subscribed.contains_key(**name))
            .map(|name| json!({"name":name,"version":1}))
            .collect();
        if !missing.is_empty() {
            let v = self
                .request(
                    Method::POST,
                    "v1/events/subscriptions",
                    Some(json!({"broadcaster_user_id":id,"events":missing,"method":"webhook"})),
                    true,
                )
                .await?;
            for v in v["data"].as_array().into_iter().flatten() {
                if let (Some(name), Some(sid)) = (v["name"].as_str(), v["subscription_id"].as_str())
                {
                    if v["error"].as_str().is_none_or(|s| s.is_empty()) {
                        subscribed.insert(name.into(), sid.into());
                    }
                }
            }
        }
        if !subscribed.contains_key("chat.message.sent") {
            return Err(
                "Kick nie potwierdził subskrypcji czatu. Sprawdź webhook w panelu aplikacji Kick."
                    .into(),
            );
        }
        let mut s = self.state.lock().await;
        for (name, sid) in subscribed {
            s.subscriptions.insert(sid, (channel.clone(), name));
        }
        s.error = None;
        drop(s);
        // Connected means subscriptions accepted; delivery is reported separately via last_event.
        let key = format!("kick:{id}");
        let _ = self.ui.send(UiEvent::Connected {
            channel: channel.clone(),
            channel_id: key.clone(),
        });
        self.refresh_roles().await;
        self.emit().await;
        if let Some(store) = &self.store {
            store.begin_channel(&key);
            let client = crate::seventv::SevenTvClient::new(
                self.http.clone(),
                EmoteScale::X1,
                crate::seventv::ImageFormat::Webp,
            );
            if let Ok(emotes) = client.channel_emotes(&key).await {
                let count = emotes.len();
                if store.set_channel(&key, emotes) {
                    let _ = self.ui.send(UiEvent::EmotesLoaded {
                        channel: Some(channel),
                        count,
                    });
                }
            }
        }
        Ok(())
    }
    pub async fn part(&self, channel: &str) {
        let _gate = self.subscription_gate.lock().await;
        let mut s = self.state.lock().await;
        let id = s.channels.remove(channel);
        s.subscriptions.retain(|_, (c, _)| c != channel);
        drop(s);
        if let Some(id) = id {
            self.native.lock().await.remove(&format!("kick:{id}"));
            if let Some(store) = &self.store {
                store.remove_channel(&format!("kick:{id}"));
            }
        }
        self.emit().await;
        // Existing app subscriptions are retained for reconnects; unopened channels are ignored.
    }
    pub async fn send(&self, channel_id: &str, text: &str) -> Result<(), String> {
        let native = self.native.lock().await;
        let text = encode_emotes(text, native.get(channel_id));
        drop(native);
        if text.trim().is_empty() || text.chars().count() > 500 {
            return Err("Wiadomość Kicka musi mieć 1–500 znaków.".into());
        }
        let response=self.request(
            Method::POST,
            "v1/chat",
            Some(json!({"broadcaster_user_id":safe_id(channel_id)?,"content":text,"type":"user"})),
            false,
        )
        .await?;
        if response["data"]["is_sent"] != true {
            return Err(
                "Kick nie potwierdził wysłania wiadomości. Sprawdź ograniczenia czatu.".into(),
            );
        }
        Ok(())
    }
    pub async fn ban(
        &self,
        channel_id: &str,
        user_id: &str,
        seconds: Option<u32>,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let mut v = json!({"broadcaster_user_id":safe_id(channel_id)?,"user_id":safe_id(user_id)?});
        if let Some(seconds) = seconds {
            v["duration"] = json!(timeout_minutes(seconds)?);
        }
        if let Some(reason) = reason {
            v["reason"] = json!(reason.chars().take(100).collect::<String>());
        }
        self.request(Method::POST, "v1/moderation/bans", Some(v), false)
            .await?;
        Ok(())
    }
    pub async fn unban(&self, channel_id: &str, user_id: &str) -> Result<(), String> {
        self.request(
            Method::DELETE,
            "v1/moderation/bans",
            Some(json!({"broadcaster_user_id":safe_id(channel_id)?,"user_id":safe_id(user_id)?})),
            false,
        )
        .await?;
        Ok(())
    }
    pub async fn delete(&self, message_id: &str) -> Result<(), String> {
        if message_id.is_empty()
            || !message_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err("Niepoprawne ID wiadomości Kicka.".into());
        }
        self.request(
            Method::DELETE,
            &format!("v1/chat/{message_id}"),
            None,
            false,
        )
        .await?;
        Ok(())
    }
    pub async fn command(self: Arc<Self>, cmd: Command) {
        let joining = matches!(&cmd, Command::Join(_));
        let channel = match &cmd {
            Command::Join(c) | Command::Part(c) => c.clone(),
            Command::SendMessage { channel, .. }
            | Command::Timeout { channel, .. }
            | Command::Ban { channel, .. }
            | Command::Unban { channel, .. }
            | Command::DeleteMessage { channel, .. } => channel.login.clone(),
            _ => return,
        };
        let result = match cmd {
            Command::Join(c) => self.join(c).await,
            Command::Part(c) => {
                self.part(&c).await;
                Ok(())
            }
            Command::SendMessage { channel, text } => self.send(&channel.id, &text).await,
            Command::Timeout {
                channel,
                user_id,
                login,
                seconds,
                reason,
            } => match self
                .ban(&channel.id, &user_id, Some(seconds), reason.as_deref())
                .await
            {
                Ok(()) => {
                    let _ = self.ui.send(UiEvent::UserCleared {
                        channel: channel.login,
                        user_id,
                        login,
                        timeout_secs: Some(timeout_minutes(seconds).unwrap_or(1) as u64 * 60),
                    });
                    Ok(())
                }
                r => r,
            },
            Command::Ban {
                channel,
                user_id,
                login,
                reason,
            } => match self
                .ban(&channel.id, &user_id, None, reason.as_deref())
                .await
            {
                Ok(()) => {
                    let _ = self.ui.send(UiEvent::UserCleared {
                        channel: channel.login,
                        user_id,
                        login,
                        timeout_secs: None,
                    });
                    Ok(())
                }
                r => r,
            },
            Command::Unban {
                channel,
                user_id,
                login,
            } => match self.unban(&channel.id, &user_id).await {
                Ok(()) => {
                    let _ = self.ui.send(UiEvent::Unbanned {
                        channel: channel.login,
                        login,
                    });
                    Ok(())
                }
                r => r,
            },
            Command::DeleteMessage {
                channel,
                message_id,
            } => match self.delete(&message_id).await {
                Ok(()) => {
                    let _ = self.ui.send(UiEvent::MessageDeleted {
                        channel: channel.login,
                        message_id,
                    });
                    Ok(())
                }
                r => r,
            },
            _ => Ok(()),
        };
        if let Err(text) = result {
            if joining {
                let _ = self.ui.send(UiEvent::Disconnected {
                    channel: channel.clone(),
                });
            }
            let _ = self.ui.send(UiEvent::Error {
                channel: Some(channel),
                text: text.clone(),
            });
            self.error(&text).await;
        }
    }
    async fn public_key(&self, force: bool) -> Result<Vec<u8>, String> {
        let mut cache = self.key.lock().await;
        if !force {
            if let Some((key, at)) = &*cache {
                if at.elapsed() < Duration::from_secs(3600) {
                    return Ok(key.clone());
                }
            }
        }
        let response = self
            .http
            .get(format!("{}/v1/public-key", self.api))
            .send()
            .await
            .map_err(|_| "Nie można pobrać klucza Kicka.")?
            .error_for_status()
            .map_err(|_| "Kick odrzucił pobranie klucza.")?;
        let v: Value = response.json().await.map_err(|_| "Błędny klucz Kicka.")?;
        let key = parse_key(
            v["data"]["public_key"]
                .as_str()
                .ok_or("Brak klucza Kicka.")?,
        )?;
        *cache = Some((key.clone(), Instant::now()));
        Ok(key)
    }
    pub async fn receive(
        &self,
        headers: &axum::http::HeaderMap,
        body: &[u8],
    ) -> Result<(), axum::http::StatusCode> {
        self.receive_at(headers, body, Utc::now()).await
    }
    async fn receive_at(
        &self,
        headers: &axum::http::HeaderMap,
        body: &[u8],
        now: chrono::DateTime<Utc>,
    ) -> Result<(), axum::http::StatusCode> {
        use axum::http::StatusCode as S;
        if !self.config.lock().await.webhooks {
            return Err(S::SERVICE_UNAVAILABLE);
        }
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .ok_or(S::UNAUTHORIZED)
        };
        let id = get("kick-event-message-id")?;
        let stamp = get("kick-event-message-timestamp")?;
        if id.len() > 128 {
            return Err(S::BAD_REQUEST);
        }
        let time = chrono::DateTime::parse_from_rfc3339(stamp).map_err(|_| S::UNAUTHORIZED)?;
        if (now - time.with_timezone(&Utc)).num_seconds().abs() > 300 {
            return Err(S::UNAUTHORIZED);
        }
        let signature = STANDARD
            .decode(get("kick-event-signature")?)
            .map_err(|_| S::UNAUTHORIZED)?;
        let mut signed = format!("{id}.{stamp}.").into_bytes();
        signed.extend_from_slice(body);
        let key = self
            .public_key(false)
            .await
            .map_err(|_| S::SERVICE_UNAVAILABLE)?;
        if !verify(&key, &signed, &signature) {
            // Refresh once on rotation; prevent unauthenticated traffic causing unbounded key fetches.
            let old = self
                .key
                .lock()
                .await
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(60));
            if !old
                || !verify(
                    &self
                        .public_key(true)
                        .await
                        .map_err(|_| S::SERVICE_UNAVAILABLE)?,
                    &signed,
                    &signature,
                )
            {
                return Err(S::UNAUTHORIZED);
            }
        }
        let event = get("kick-event-type")?;
        let sid = get("kick-event-subscription-id")?;
        if get("kick-event-version")? != "1" {
            return Err(S::BAD_REQUEST);
        }
        let payload: Value = serde_json::from_slice(body).map_err(|_| S::BAD_REQUEST)?;
        let mut seen = self.seen.lock().await;
        if seen.iter().any(|s| s == id) {
            return Ok(());
        }
        let mut s = self.state.lock().await;
        let Some((channel, expected)) = s.subscriptions.get(sid).cloned() else {
            return Ok(());
        };
        if expected != event
            || number(&payload["broadcaster"]["user_id"]) != s.channels.get(&channel).copied()
        {
            return Err(S::BAD_REQUEST);
        }
        seen.push_back(id.into());
        if seen.len() > 4096 {
            seen.pop_front();
        }
        drop(seen);
        s.last_event = Some(Utc::now().timestamp_millis());
        let emit = s
            .last_status
            .is_none_or(|t| t.elapsed() > Duration::from_secs(5));
        if emit {
            s.last_status = Some(Instant::now());
        }
        drop(s);
        if event == "chat.message.sent" {
            let s = self.state.lock().await;
            if s.session
                .as_ref()
                .is_some_and(|u| Some(u.user_id) == number(&payload["sender"]["user_id"]))
            {
                let can = s.channels.get(&channel).copied()
                    == number(&payload["sender"]["user_id"])
                    || payload["sender"]["identity"]["badges"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|b| matches!(b["type"].as_str(), Some("moderator" | "broadcaster")));
                let _ = self.ui.send(UiEvent::ModStatus {
                    channel: channel.clone(),
                    can_moderate: can,
                });
            }
        }
        for event in translate(event, id, &channel, &payload, self.store.as_deref()) {
            if let UiEvent::Message(m) = &event {
                let mut native = self.native.lock().await;
                let map = native.entry(m.channel_id.clone()).or_default();
                for segment in &m.segments {
                    if let Segment::Emote(e) = segment {
                        if e.base.provider == crate::emotes::EmoteProvider::Kick && map.len() < 500
                        {
                            map.insert(e.base.name.to_string(), e.base.id.to_string());
                        }
                    }
                }
            }
            let _ = self.ui.send(event);
        }
        if emit {
            self.emit().await;
        }
        Ok(())
    }
}

fn encode_emotes(text: &str, native: Option<&HashMap<String, String>>) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|part| {
            let word = part.trim_end_matches(char::is_whitespace);
            match native.and_then(|n| n.get(word)) {
                Some(id) => format!("[emote:{id}:{word}]{}", &part[word.len()..]),
                None => part.into(),
            }
        })
        .collect()
}

pub fn timeout_minutes(seconds: u32) -> Result<u32, String> {
    if seconds == 0 || seconds > 604800 {
        return Err("Kick: timeout musi trwać od 1 sekundy do 7 dni; jest zaokrąglany w górę do pełnej minuty.".into());
    }
    Ok(seconds.div_ceil(60))
}
fn parse_key(pem: &str) -> Result<Vec<u8>, String> {
    if pem.len() > 16384 {
        return Err("Nieprawidłowy klucz Kicka.".into());
    }
    let der = STANDARD
        .decode(
            pem.lines()
                .filter(|l| !l.starts_with("-----"))
                .collect::<String>(),
        )
        .map_err(|_| "Błędny klucz PEM Kicka.")?;
    let key =
        spki::SubjectPublicKeyInfoRef::from_der(&der).map_err(|_| "Błędny klucz SPKI Kicka.")?;
    if key.algorithm.oid.to_string() != "1.2.840.113549.1.1.1" {
        return Err("Klucz Kicka nie jest RSA.".into());
    }
    key.subject_public_key
        .as_bytes()
        .map(|b| b.to_vec())
        .ok_or("Błędny klucz RSA Kicka.".into())
}
fn verify(key: &[u8], body: &[u8], signature: &[u8]) -> bool {
    ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, key)
        .verify(body, signature)
        .is_ok()
}
async fn webhook(
    axum::extract::State(client): axum::extract::State<Arc<Client>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::http::StatusCode {
    match client.receive(&headers, &body).await {
        Ok(()) => axum::http::StatusCode::NO_CONTENT,
        Err(code) => code,
    }
}

pub fn translate(
    event: &str,
    event_id: &str,
    channel: &str,
    v: &Value,
    store: Option<&EmoteStore>,
) -> Vec<UiEvent> {
    let at = v["created_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    if event == "chat.message.sent" {
        let Some(content) = v["content"].as_str().filter(|s| s.len() <= 16384) else {
            return vec![];
        };
        let sender = &v["sender"];
        let id = format!("kick:{}", number(&v["broadcaster"]["user_id"]).unwrap_or(0));
        let map = store.map(|s| s.map_for(&id)).unwrap_or_default();
        let mut segments = smallvec::SmallVec::new();
        let mut plain = String::new();
        // Native Kick emotes can occur adjacent to text; recognize only numeric IDs.
        let mut rest = content;
        while let Some(start) = rest.find("[emote:") {
            plain.push_str(&rest[..start]);
            let tail = &rest[start + 7..];
            if let Some(end) = tail.find(']') {
                if let Some((eid, name)) = tail[..end].split_once(':') {
                    if !eid.is_empty()
                        && eid.len() < 24
                        && eid.bytes().all(|b| b.is_ascii_digit())
                        && !name.is_empty()
                    {
                        segments.extend(parser::tokenize(&plain, &[], &map, EmoteScale::X1));
                        plain.clear();
                        let e = crate::emotes::Emote {
                            id: eid.into(),
                            name: name.into(),
                            url: format!("https://files.kick.com/emotes/{eid}/fullsize").into(),
                            width: 28.,
                            height: 28.,
                            animated: true,
                            zero_width: false,
                            provider: crate::emotes::EmoteProvider::Kick,
                        };
                        segments.push(Segment::Emote(parser::EmoteSegment {
                            base: Arc::new(e),
                            overlays: Default::default(),
                        }));
                        rest = &tail[end + 1..];
                        continue;
                    }
                }
            }
            plain.push_str("[emote:");
            rest = tail;
        }
        plain.push_str(rest);
        segments.extend(parser::tokenize(&plain, &[], &map, EmoteScale::X1));
        let color = sender["identity"]["username_color"]
            .as_str()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches('#'), 16).ok())
            .map(|c| {
                [
                    ((c >> 16) & 255) as u8,
                    ((c >> 8) & 255) as u8,
                    (c & 255) as u8,
                ]
            });
        let badges = sender["identity"]["badges"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| {
                Some(Badge {
                    name: b["type"].as_str()?.into(),
                    version: "1".into(),
                })
            })
            .collect();
        return vec![UiEvent::Message(Box::new(ParsedMessage {
            id: v["message_id"].as_str().unwrap_or(event_id).into(),
            channel_login: channel.into(),
            channel_id: id,
            sender_id: number(&sender["user_id"]).unwrap_or(0).to_string(),
            sender_login: account(sender),
            sender_name: sender["username"].as_str().unwrap_or("Anonim").into(),
            color,
            badges,
            is_action: false,
            timestamp: at,
            segments,
            raw_text: content.into(),
            deleted: false,
        }))];
    }
    if event == "moderation.banned" {
        let user = &v["banned_user"];
        let expires = v["metadata"]["expires_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        let created = v["metadata"]["created_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        return vec![UiEvent::UserCleared {
            channel: channel.into(),
            user_id: number(&user["user_id"]).unwrap_or(0).to_string(),
            login: account(user),
            timeout_secs: expires.map(|t| {
                (t - created.unwrap_or_else(|| at.fixed_offset()))
                    .num_seconds()
                    .max(1) as u64
            }),
        }];
    }
    let (user, kind) = match event {
        "channel.followed" => (&v["follower"], AlertKind::Follow),
        "channel.subscription.new" | "channel.subscription.renewal" => (
            &v["subscriber"],
            AlertKind::Sub {
                tier: "Kick".into(),
                months: number(&v["duration"]).unwrap_or(1),
                resub: event.ends_with("renewal"),
            },
        ),
        "channel.subscription.gifts" => (
            &v["gifter"],
            AlertKind::MassGift {
                tier: "Kick".into(),
                count: v["giftees"].as_array().map(|a| a.len() as u64).unwrap_or(0),
                total: None,
                anonymous: v["gifter"]["is_anonymous"].as_bool().unwrap_or(false),
            },
        ),
        "kicks.gifted" => (
            &v["sender"],
            AlertKind::Kicks {
                amount: number(&v["gift"]["amount"]).unwrap_or(0),
            },
        ),
        "channel.reward.redemption.updated" if v["status"] == "pending" => (
            &v["redeemer"],
            AlertKind::Redemption {
                reward: v["reward"]["title"].as_str().unwrap_or("Nagroda").into(),
                cost: number(&v["reward"]["cost"]).unwrap_or(0),
                input: v["user_input"].as_str().map(str::to_owned),
            },
        ),
        _ => return vec![],
    };
    vec![UiEvent::Alert(Box::new(ChatAlert {
        id: format!("kick:{event_id}"),
        channel_login: channel.into(),
        user_id: number(&user["user_id"]).unwrap_or(0).to_string(),
        user_login: account(user),
        user_name: user["username"].as_str().unwrap_or("Anonim").into(),
        color: None,
        kind,
        message: None,
        timestamp: at,
    }))]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kick_timeout_uses_minutes_not_twitch_seconds() {
        assert_eq!(timeout_minutes(1).unwrap(), 1);
        assert_eq!(timeout_minutes(61).unwrap(), 2);
        assert_eq!(timeout_minutes(600).unwrap(), 10);
        assert!(timeout_minutes(604801).is_err());
    }
    #[test]
    fn official_chat_payload_preserves_platform_and_native_emotes() {
        let v = json!({"message_id":"m1","broadcaster":{"user_id":123},"sender":{"user_id":456,"username":"Name","channel_slug":"name","identity":{"username_color":"#123456","badges":[{"type":"moderator"}]}},"content":"Hi[emote:37226:KEKW] !odp A","created_at":"2026-11-10T10:00:00Z"});
        let events = translate("chat.message.sent", "event", "kick:same", &v, None);
        let UiEvent::Message(m) = &events[0] else {
            panic!()
        };
        assert_eq!(m.channel_login, "kick:same");
        assert_eq!(m.channel_id, "kick:123");
        assert_eq!(m.sender_login, "name");
        assert!(matches!(&m.segments[1], Segment::Emote(_)));
        assert_eq!(m.badges[0].name, "moderator");
    }
}
