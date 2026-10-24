//! Serwer panelu: pliki strony + WebSocket, wyłącznie na 127.0.0.1.
//!
//! Zabezpieczenia:
//! * nasłuch tylko na 127.0.0.1 — inne komputery w sieci nie mają dostępu,
//! * nagłówek `Host` musi wskazywać na 127.0.0.1/localhost (ochrona przed
//!   DNS rebinding),
//! * nagłówek `Origin` (wysyłany przez przeglądarki) musi wskazywać na
//!   ten panel — dowolna strona otwarta w przeglądarce nie połączy się
//!   z WebSocketem i nie wyśle np. bana w Twoim imieniu.

use std::sync::Arc;

use axum::{
    Router,
    extract::{
        Path, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, watch};

use crate::{
    demo,
    engine::{Control, ControlState},
    hub::HubHandle,
    protocol::ClientMsg,
};

/// Wspólny stan serwera.
#[derive(Clone)]
struct App {
    hub: HubHandle,
    /// Zamknięcie silnika / przekazanie pracy — rozłącza też panele i overlaye.
    shutdown: watch::Receiver<bool>,
    control: ControlState,
    media: Arc<crate::media::Media>,
}

static INDEX: &str = include_str!("../web/index.html");
static APP_JS: &str = include_str!("../web/app.js");
static APP_CSS: &str = include_str!("../web/app.css");
static OVERLAY: &str = include_str!("../web/overlay.html");
static LOGO: &[u8] = include_bytes!("../../assets/logo.png");
static FONTS: &[(&str, &[u8])] = &[
    ("Inter-Regular.ttf", include_bytes!("../../assets/fonts/Inter-Regular.ttf")),
    ("Inter-SemiBold.ttf", include_bytes!("../../assets/fonts/Inter-SemiBold.ttf")),
    ("Outfit-Bold.ttf", include_bytes!("../../assets/fonts/Outfit-Bold.ttf")),
    ("JetBrainsMono-Medium.ttf", include_bytes!("../../assets/fonts/JetBrainsMono-Medium.ttf")),
];

pub async fn serve(listener: tokio::net::TcpListener, hub: HubHandle, mut shutdown: watch::Receiver<bool>, control: ControlState) {
    let port = crate::PORT;
    let state = App { hub, shutdown: shutdown.clone(), control, media: Arc::new(crate::media::Media::new()) };
    let app = Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX.as_bytes(), false) }))
        .route("/app.js", get(|| async { asset("text/javascript; charset=utf-8", APP_JS.as_bytes(), false) }))
        .route("/app.css", get(|| async { asset("text/css; charset=utf-8", APP_CSS.as_bytes(), false) }))
        .route("/logo.png", get(|| async { asset("image/png", LOGO, true) }))
        .route("/fonts/{name}", get(font))
        .route("/demo/{name}", get(demo_emote))
        .route("/ws", get(websocket))
        .route("/overlay", get(|| async { asset("text/html; charset=utf-8", OVERLAY.as_bytes(), false) }))
        .route("/overlay/ws", get(overlay_socket))
        .route("/media", get(media))
        .route("/control/health", get(health))
        .route("/control/adopt", axum::routing::post(adopt))
        .route("/control/shutdown", axum::routing::post(shutdown_engine))
        .layer(middleware::from_fn(move |req, next| guard(port, req, next)))
        .with_state(state);

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.changed().await;
        })
        .await;
    if let Err(e) = result {
        tracing::error!("serwer panelu zakończył się błędem: {e}");
    }
}

/// Odrzuca żądania spoza panelu (Host/Origin) — patrz opis modułu.
async fn guard(port: u16, req: Request, next: Next) -> Response {
    if !is_local_request(port, req.headers()) {
        tracing::warn!("odrzucono żądanie spoza panelu: {:?}", req.headers().get(header::ORIGIN));
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

fn is_local_request(port: u16, headers: &HeaderMap) -> bool {
    let allowed_hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let host_ok = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| allowed_hosts.iter().any(|a| a == h));
    let origin_ok = match headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        None => true, // zwykłe wejście na stronę (bez Origin) albo panel OBS
        Some(origin) => allowed_hosts.iter().any(|a| origin == format!("http://{a}")),
    };
    host_ok && origin_ok
}

fn asset(content_type: &'static str, body: &'static [u8], cacheable: bool) -> Response {
    let cache = if cacheable { "public, max-age=86400" } else { "no-cache" };
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, cache)], body).into_response()
}

async fn font(Path(name): Path<String>) -> Response {
    match FONTS.iter().find(|(n, _)| *n == name) {
        Some((_, bytes)) => asset("font/ttf", bytes, true),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn demo_emote(Path(name): Path<String>) -> Response {
    let name = name.trim_end_matches(".svg");
    match demo::svg(name) {
        Some(svg) => ([(header::CONTENT_TYPE, "image/svg+xml")], svg).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct MediaQuery {
    u: String,
}

/// Obrazek z linku na czacie (kappa.lol), pobrany przez silnik — patrz `media.rs`.
async fn media(State(app): State<App>, axum::extract::Query(q): axum::extract::Query<MediaQuery>) -> Response {
    match app.media.get(&q.u, demo::enabled()).await {
        Ok((bytes, ctype)) => (
            [
                (header::CONTENT_TYPE, ctype),
                (header::CACHE_CONTROL, "private, max-age=3600"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::CONTENT_SECURITY_POLICY, "default-src 'none'"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::debug!("podgląd {}: {e}", q.u);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

// ── Sterowanie silnikiem (wtyczka OBS, aktualizacje) ──────────────────

/// Bez tokenu — mówi tylko, że to LiMBOCHAT i jaka wersja.
async fn health() -> Response {
    let body = serde_json::json!({ "app": "limbochat", "version": crate::version(), "pid": std::process::id() });
    ([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], body.to_string()).into_response()
}

fn token_ok(app: &App, headers: &HeaderMap) -> bool {
    let given = headers.get("x-limbochat-token").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let expected = app.control.token.as_bytes();
    // Porównanie w stałym czasie.
    given.len() == expected.len() && given.bytes().zip(expected).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

#[derive(serde::Deserialize)]
struct AdoptQuery {
    pid: u32,
}

/// Nowo uruchomiony OBS „przejmuje” działający silnik.
async fn adopt(State(app): State<App>, headers: HeaderMap, axum::extract::Query(q): axum::extract::Query<AdoptQuery>) -> Response {
    if !token_ok(&app, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    app.control.obs_pid.store(q.pid, std::sync::atomic::Ordering::Relaxed);
    tracing::info!("silnik przejęty przez OBS (pid {})", q.pid);
    StatusCode::NO_CONTENT.into_response()
}

async fn shutdown_engine(State(app): State<App>, headers: HeaderMap) -> Response {
    if !token_ok(&app, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let _ = app.control.tx.send(Control::Exit("zamknięcie OBS"));
    StatusCode::NO_CONTENT.into_response()
}

async fn websocket(ws: WebSocketUpgrade, State(app): State<App>) -> Response {
    ws.on_upgrade(move |socket| connection(socket, app.hub, app.shutdown))
}

async fn connection(socket: WebSocket, hub: HubHandle, mut shutdown: watch::Receiver<bool>) {
    let Some((snapshot, mut events)) = hub.connect().await else { return };
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Arc<str>>();

    let text = |json: Arc<str>| Message::Text(json.as_ref().to_owned().into());
    if sink.send(text(snapshot)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            event = events.recv() => match event {
                Ok(json) => {
                    if sink.send(text(json)).await.is_err() {
                        break;
                    }
                }
                // Panel nie nadążał (np. był ukryty przy ogromnym czacie) — wyślij świeżą migawkę.
                Err(broadcast::error::RecvError::Lagged(_)) => match hub.connect().await {
                    Some((snapshot, rx)) => {
                        events = rx;
                        if sink.send(text(snapshot)).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                Err(broadcast::error::RecvError::Closed) => break,
            },
            Some(json) = out_rx.recv() => {
                if sink.send(text(json)).await.is_err() {
                    break;
                }
            }
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(raw))) => match serde_json::from_str::<ClientMsg>(raw.as_str()) {
                    Ok(msg) => hub.client(msg, out_tx.clone()),
                    Err(e) => tracing::warn!("niezrozumiała wiadomość panelu: {e}"),
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

// ── Overlay na stream ──────────────────────────────────────────────────
//
// Osobny WebSocket tylko do odczytu: dostaje wyłącznie zdarzenia LimboAI
// i wygląd (kolor, pozycja). Nie widzi czatu, kont ani ustawień, nie może
// niczego wysłać do panelu.

async fn overlay_socket(ws: WebSocketUpgrade, State(app): State<App>) -> Response {
    ws.on_upgrade(move |socket| overlay_connection(socket, app.hub, app.shutdown))
}

/// Wiadomość „wygląd overlayu” z ustawień panelu.
fn overlay_look(settings: &serde_json::Value) -> Arc<str> {
    serde_json::json!({ "t": "overlay", "accent": settings["accent"], "overlay": settings["overlay"], "version": crate::version() })
        .to_string()
        .into()
}

/// Czy zdarzenie z panelu ma trafić do overlayu (i w jakiej postaci).
fn overlay_filter(json: &str) -> Option<Arc<str>> {
    // Szybki odsiew: większość ruchu to wiadomości czatu.
    if !json.starts_with(r#"{"t":"jarvis""#) && !json.starts_with(r#"{"t":"settings""#) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    match value["t"].as_str()? {
        "settings" => Some(overlay_look(&value)),
        "jarvis" => {
            const PUBLIC: [&str; 7] = ["state", "level", "transcript", "action", "reply", "error", "reminder"];
            let ev = &value["ev"];
            let kind = ev["e"].as_str()?;
            if !PUBLIC.contains(&kind) {
                return None;
            }
            // Błędy nie trafiają na stream z treścią — overlay tylko się chowa.
            let ev = if kind == "error" { serde_json::json!({ "e": "error" }) } else { ev.clone() };
            Some(serde_json::json!({ "t": "jarvis", "ev": ev }).to_string().into())
        }
        _ => None,
    }
}

async fn overlay_connection(socket: WebSocket, hub: HubHandle, mut shutdown: watch::Receiver<bool>) {
    let Some((snapshot, mut events)) = hub.connect().await else { return };
    let (mut sink, mut stream) = socket.split();
    let settings = serde_json::from_str::<serde_json::Value>(&snapshot).map(|v| v["settings"].clone()).unwrap_or_default();
    let text = |json: Arc<str>| Message::Text(json.as_ref().to_owned().into());
    if sink.send(text(overlay_look(&settings))).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            event = events.recv() => match event {
                Ok(json) => {
                    if let Some(out) = overlay_filter(&json) {
                        if sink.send(text(out)).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {} // overlay nic nie wysyła — ignorujemy
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        if let Some(o) = origin {
            h.insert(header::ORIGIN, HeaderValue::from_str(o).unwrap());
        }
        h
    }

    #[test]
    fn accepts_only_the_panel_itself() {
        assert!(is_local_request(47250, &headers("127.0.0.1:47250", None)));
        assert!(is_local_request(47250, &headers("127.0.0.1:47250", Some("http://127.0.0.1:47250"))));
        assert!(is_local_request(47250, &headers("localhost:47250", Some("http://localhost:47250"))));
    }

    #[test]
    fn overlay_gets_only_public_assistant_events() {
        assert!(overlay_filter(r#"{"t":"message","m":{"raw":"sekret"}}"#).is_none());
        assert!(overlay_filter(r#"{"t":"jarvis","ev":{"e":"status","config":{"mic":"Mikrofon"}}}"#).is_none());
        assert!(overlay_filter(r#"{"t":"jarvis","ev":{"e":"enroll","step":1}}"#).is_none());
        let reply = overlay_filter(r#"{"t":"jarvis","ev":{"e":"reply","text":"Gotowe"}}"#).unwrap();
        assert!(reply.contains("Gotowe"));
        let err = overlay_filter(r#"{"t":"jarvis","ev":{"e":"error","text":"Klucz Gemini jest nieprawidłowy"}}"#).unwrap();
        assert!(!err.contains("Klucz"), "treść błędu nie idzie na stream");
        let look = overlay_filter(r##"{"t":"settings","accent":"#ff0000","channels":["a"],"overlay":{"position":"top-left"}}"##).unwrap();
        assert!(look.contains("top-left") && look.contains("#ff0000") && !look.contains("channels"));
    }

    #[test]
    fn rejects_other_websites_and_dns_rebinding() {
        assert!(!is_local_request(47250, &headers("127.0.0.1:47250", Some("https://evil.example"))));
        assert!(!is_local_request(47250, &headers("evil.example:47250", None)), "DNS rebinding");
        assert!(!is_local_request(47250, &headers("127.0.0.1:47250", Some("null"))));
    }
}
