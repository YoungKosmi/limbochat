//! Twitch EventSub przez WebSocket: follow i nagrody za punkty kanału.
//!
//! IRC (czytany anonimowo) daje bitsy, suby, gifty i raidy, ale nie follow
//! ani nagród — te przychodzą tylko przez EventSub i tylko dla kanału
//! zalogowanego streamera (wymagają uprawnień `moderator:read:followers`
//! i `channel:read:redemptions`).
//!
//! Przebieg: połączenie z `wss://eventsub.wss.twitch.tv/ws` → `session_welcome`
//! z identyfikatorem sesji → subskrypcje przez Helix → powiadomienia.
//! Twitch wysyła `session_keepalive` — jego brak = zerwane połączenie, łączymy
//! się od nowa (z rosnącą przerwą). `session_reconnect` = przejście na nowy adres
//! (subskrypcje zostają).

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::tungstenite::Message;

use crate::events::{AlertKind, ChatAlert, UiEvent};

pub const FOLLOW: &str = "channel.follow";
pub const REDEMPTION: &str = "channel.channel_points_custom_reward_redemption.add";

/// Tworzy subskrypcję danego typu dla sesji (w silniku: Helix; w testach: atrapa).
pub type Subscriber = Arc<dyn Fn(&'static str, String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

fn default_url() -> String {
    std::env::var("LIMBOCHAT_EVENTSUB_URL").unwrap_or_else(|_| "wss://eventsub.wss.twitch.tv/ws".to_owned())
}

/// Uruchamia nasłuch; zatrzymanie = `abort()` na zwróconym zadaniu.
pub fn spawn(types: Vec<&'static str>, channel_login: String, subscribe: Subscriber, ui: mpsc::UnboundedSender<UiEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        if types.is_empty() {
            return;
        }
        let mut seen = VecDeque::new();
        let mut backoff = Duration::from_secs(2);
        loop {
            match session(&default_url(), &types, &channel_login, &subscribe, &ui, &mut seen).await {
                Ok(()) => backoff = Duration::from_secs(2),
                Err(e) => tracing::warn!("EventSub: {e} — ponowna próba za {} s", backoff.as_secs()),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    })
}

async fn session(
    start_url: &str,
    types: &[&'static str],
    channel_login: &str,
    subscribe: &Subscriber,
    ui: &mpsc::UnboundedSender<UiEvent>,
    seen: &mut VecDeque<String>,
) -> Result<(), String> {
    let mut url = start_url.to_owned();
    let mut subscribed = false;
    loop {
        let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await.map_err(|e| format!("połączenie: {e}"))?;
        let (mut sink, mut stream) = ws.split();

        // Powitanie z identyfikatorem sesji.
        let welcome = tokio::time::timeout(Duration::from_secs(15), next_json(&mut stream))
            .await
            .map_err(|_| "brak powitania".to_owned())??;
        if welcome["metadata"]["message_type"] != "session_welcome" {
            return Err("nieoczekiwana pierwsza wiadomość".into());
        }
        let session = &welcome["payload"]["session"];
        let session_id = session["id"].as_str().unwrap_or_default().to_owned();
        let keepalive = Duration::from_secs(session["keepalive_timeout_seconds"].as_u64().unwrap_or(10).clamp(5, 600));

        if !subscribed {
            for kind in types {
                if let Err(e) = subscribe(kind, session_id.clone()).await {
                    tracing::warn!("EventSub: subskrypcja {kind}: {e}");
                    let what = if *kind == FOLLOW { "follow" } else { "nagród za punkty" };
                    let _ = ui.send(UiEvent::Notice {
                        channel: Some(channel_login.to_owned()),
                        text: format!("Alerty {what} niedostępne: {e}. Zaloguj się ponownie, jeśli to nowa funkcja."),
                    });
                }
            }
            subscribed = true;
            tracing::info!("EventSub: nasłuch {types:?} na #{channel_login}");
        }

        // Powiadomienia aż do zerwania albo przeniesienia na nowy adres.
        loop {
            let next = tokio::time::timeout(keepalive + Duration::from_secs(10), stream.next())
                .await
                .map_err(|_| "brak keepalive".to_owned())?;
            let text = match next {
                None => return Err("połączenie zamknięte".into()),
                Some(Err(e)) => return Err(format!("błąd połączenia: {e}")),
                Some(Ok(Message::Ping(p))) => {
                    let _ = sink.send(Message::Pong(p)).await;
                    continue;
                }
                Some(Ok(Message::Close(_))) => return Err("Twitch zamknął połączenie".into()),
                Some(Ok(Message::Text(t))) => t,
                Some(Ok(_)) => continue,
            };
            let Ok(msg) = serde_json::from_str::<Value>(text.as_str()) else { continue };
            match msg["metadata"]["message_type"].as_str().unwrap_or_default() {
                "notification" => {
                    let id = msg["metadata"]["message_id"].as_str().unwrap_or_default().to_owned();
                    if seen.contains(&id) {
                        continue; // Twitch może powtórzyć wiadomość
                    }
                    seen.push_back(id.clone());
                    if seen.len() > 100 {
                        seen.pop_front();
                    }
                    let kind = msg["payload"]["subscription"]["type"].as_str().unwrap_or_default();
                    if let Some(alert) = to_alert(kind, &msg["payload"]["event"], &id, channel_login) {
                        let _ = ui.send(UiEvent::Alert(Box::new(alert)));
                    }
                }
                "session_reconnect" => {
                    if let Some(new) = msg["payload"]["session"]["reconnect_url"].as_str() {
                        url = new.to_owned();
                        break; // nowe połączenie; subskrypcje przechodzą same
                    }
                }
                "revocation" => {
                    tracing::warn!("EventSub: Twitch cofnął subskrypcję {}", msg["payload"]["subscription"]["type"]);
                }
                _ => {} // session_keepalive
            }
        }
    }
}

async fn next_json<S>(stream: &mut S) -> Result<Value, String>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(msg) = stream.next().await {
        match msg.map_err(|e| e.to_string())? {
            Message::Text(t) => return serde_json::from_str(t.as_str()).map_err(|e| e.to_string()),
            Message::Close(_) => break,
            _ => continue,
        }
    }
    Err("połączenie zamknięte".into())
}

pub fn to_alert(kind: &str, event: &Value, id: &str, channel_login: &str) -> Option<ChatAlert> {
    let s = |k: &str| event[k].as_str().unwrap_or_default().to_owned();
    let kind = match kind {
        FOLLOW => AlertKind::Follow,
        REDEMPTION => AlertKind::Redemption {
            reward: event["reward"]["title"].as_str().unwrap_or("nagroda").to_owned(),
            cost: event["reward"]["cost"].as_u64().unwrap_or(0),
            input: event["user_input"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned),
        },
        _ => return None,
    };
    let user_name = s("user_name");
    if user_name.is_empty() {
        return None;
    }
    Some(ChatAlert {
        id: format!("es-{id}"),
        channel_login: channel_login.to_owned(),
        user_id: s("user_id"),
        user_login: s("user_login"),
        user_name,
        color: None,
        kind,
        message: None,
        timestamp: chrono::Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::net::TcpListener;

    #[test]
    fn parses_follow_and_redemption() {
        let follow = to_alert(FOLLOW, &json!({ "user_id": "1", "user_login": "nowy", "user_name": "Nowy", "followed_at": "x" }), "m1", "kanal").unwrap();
        assert_eq!(follow.kind, AlertKind::Follow);
        assert_eq!(follow.user_name, "Nowy");
        let r = to_alert(REDEMPTION, &json!({ "user_id": "2", "user_login": "w", "user_name": "W", "user_input": " zagraj w GTA ", "reward": { "title": "Wybierz grę", "cost": 5000 } }), "m2", "kanal").unwrap();
        assert_eq!(r.kind, AlertKind::Redemption { reward: "Wybierz grę".into(), cost: 5000, input: Some("zagraj w GTA".into()) });
        assert!(to_alert("channel.update", &json!({}), "m3", "kanal").is_none());
    }

    /// Pełna sesja z atrapą serwera Twitcha: powitanie → subskrypcje →
    /// powiadomienie (i jego powtórka) → przeniesienie na nowy adres → kolejne powiadomienie.
    #[tokio::test]
    async fn websocket_session_with_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let note = |id: &str, kind: &str, event: Value| {
                json!({ "metadata": { "message_id": id, "message_type": "notification" },
                        "payload": { "subscription": { "type": kind }, "event": event } }).to_string()
            };
            // 1. połączenie
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let welcome = json!({ "metadata": { "message_type": "session_welcome" }, "payload": { "session": { "id": "S1", "keepalive_timeout_seconds": 10 } } });
            ws.send(Message::Text(welcome.to_string().into())).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let follow = note("n1", FOLLOW, json!({ "user_id": "1", "user_login": "nowy", "user_name": "Nowy" }));
            ws.send(Message::Text(follow.clone().into())).await.unwrap();
            ws.send(Message::Text(follow.into())).await.unwrap(); // powtórka — ma być pominięta
            ws.send(Message::Text(json!({ "metadata": { "message_type": "session_keepalive" }, "payload": {} }).to_string().into())).await.unwrap();
            let reconnect = json!({ "metadata": { "message_type": "session_reconnect" },
                                    "payload": { "session": { "id": "S1", "reconnect_url": format!("ws://{addr}/nowy") } } });
            ws.send(Message::Text(reconnect.to_string().into())).await.unwrap();
            // 2. połączenie po przeniesieniu
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws2 = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let welcome2 = json!({ "metadata": { "message_type": "session_welcome" }, "payload": { "session": { "id": "S2", "keepalive_timeout_seconds": 10 } } });
            ws2.send(Message::Text(welcome2.to_string().into())).await.unwrap();
            let redeem = note("n2", REDEMPTION, json!({ "user_id": "2", "user_login": "w", "user_name": "W", "reward": { "title": "Nawodnij się", "cost": 100 } }));
            ws2.send(Message::Text(redeem.into())).await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });

        let calls = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let log = calls.clone();
        let subscribe: Subscriber = Arc::new(move |kind, session| {
            log.lock().unwrap().push((kind.to_owned(), session));
            Box::pin(async { Ok(()) })
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut seen = VecDeque::new();
        let url = format!("ws://{addr}/ws");
        let run = tokio::spawn(async move { session(&url, &[FOLLOW, REDEMPTION], "kanal", &subscribe, &tx, &mut seen).await });

        let mut alerts = Vec::new();
        while alerts.len() < 2 {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap() {
                UiEvent::Alert(a) => alerts.push(a.kind),
                other => panic!("nieoczekiwane: {other:?}"),
            }
        }
        assert_eq!(alerts[0], AlertKind::Follow);
        assert!(matches!(&alerts[1], AlertKind::Redemption { reward, cost: 100, .. } if reward == "Nawodnij się"));
        assert!(tokio::time::timeout(Duration::from_millis(300), rx.recv()).await.is_err(), "powtórka pominięta");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls, vec![(FOLLOW.into(), "S1".into()), (REDEMPTION.into(), "S1".into())], "subskrypcje tylko raz, z pierwszą sesją");
        run.abort();
        server.abort();
    }
}
