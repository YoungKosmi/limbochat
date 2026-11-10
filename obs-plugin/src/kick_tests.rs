use super::*;
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};

fn fixture() -> (Arc<Client>, mpsc::UnboundedReceiver<UiEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Client::new(
            Config {
                client_id: "fixture-app".into(),
                webhooks: true,
            },
            tx,
            None,
            false,
        ),
        rx,
    )
}
fn signed() -> (Value, axum::http::HeaderMap) {
    let v: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/kick-webhook.json")).unwrap();
    let mut headers = axum::http::HeaderMap::new();
    for (name, value) in [
        ("kick-event-message-id", "event1"),
        (
            "kick-event-message-timestamp",
            v["timestamp"].as_str().unwrap(),
        ),
        ("kick-event-signature", v["signature"].as_str().unwrap()),
        ("kick-event-type", "chat.message.sent"),
        ("kick-event-subscription-id", "sub1"),
        ("kick-event-version", "1"),
    ] {
        headers.insert(name, value.parse().unwrap());
    }
    (v, headers)
}

#[tokio::test]
async fn signed_webhook_reaches_chat_once_and_rejects_tampering_stale_and_cross_channel() {
    let (client, mut rx) = fixture();
    let (v, mut headers) = signed();
    *client.key.lock().await = Some((
        parse_key(v["public_key"].as_str().unwrap()).unwrap(),
        Instant::now(),
    ));
    let mut state = client.state.lock().await;
    state.channels.insert("kick:same".into(), 123);
    state.subscriptions.insert(
        "sub1".into(),
        ("kick:same".into(), "chat.message.sent".into()),
    );
    drop(state);
    let now = chrono::DateTime::parse_from_rfc3339(v["timestamp"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    let body = v["body"].as_str().unwrap().as_bytes();
    assert_eq!(
        client.receive_at(&headers, b"{}", now).await,
        Err(axum::http::StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        client
            .receive_at(&headers, body, now + chrono::Duration::minutes(6))
            .await,
        Err(axum::http::StatusCode::UNAUTHORIZED)
    );
    headers.insert("kick-event-type", "channel.followed".parse().unwrap());
    assert_eq!(
        client.receive_at(&headers, body, now).await,
        Err(axum::http::StatusCode::BAD_REQUEST)
    );
    headers.insert("kick-event-type", "chat.message.sent".parse().unwrap());
    client
        .state
        .lock()
        .await
        .channels
        .insert("kick:same".into(), 999);
    assert_eq!(
        client.receive_at(&headers, body, now).await,
        Err(axum::http::StatusCode::BAD_REQUEST)
    );
    client
        .state
        .lock()
        .await
        .channels
        .insert("kick:same".into(), 123);
    client.receive_at(&headers, body, now).await.unwrap();
    let UiEvent::Message(message) = rx.try_recv().unwrap() else {
        panic!("expected chat")
    };
    assert_eq!(message.channel_login, "kick:same");
    assert_eq!(message.raw_text, "!odp A");
    while rx.try_recv().is_ok() {}
    client.receive_at(&headers, body, now).await.unwrap();
    assert!(rx.try_recv().is_err());
    *client.config.lock().await = Config::default();
    assert_eq!(
        client.receive_at(&headers, body, now).await,
        Err(axum::http::StatusCode::SERVICE_UNAVAILABLE)
    );
}

#[tokio::test]
async fn api_uses_numeric_ids_minutes_and_correct_account_token() {
    type Log = Arc<Mutex<Vec<(Value, String)>>>;
    async fn record(
        State(log): State<Log>,
        headers: axum::http::HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let sent = body["content"] != "blocked";
        log.lock()
            .await
            .push((body, headers["authorization"].to_str().unwrap().into()));
        Json(json!({"data":{"is_sent":sent}}))
    }
    let log: Log = Default::default();
    let app = Router::new()
        .route("/v1/chat", post(record))
        .route("/v1/moderation/bans", post(record).delete(record))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (mut client, _) = fixture();
    Arc::get_mut(&mut client).unwrap().api = format!("http://{addr}");
    client.state.lock().await.session = Some(Session {
        access_token: "fixture-user".into(),
        refresh_token: "fixture-refresh".into(),
        expires: Utc::now().timestamp() + 3600,
        login: "me".into(),
        user_id: 1,
    });
    client.send("kick:123", "hello").await.unwrap();
    client
        .ban("kick:123", "456", Some(61), Some("reason"))
        .await
        .unwrap();
    client.unban("kick:123", "456").await.unwrap();
    let requests = log.lock().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].0,
        json!({"broadcaster_user_id":123,"content":"hello","type":"user"})
    );
    assert_eq!(requests[1].0["duration"], 2);
    assert_eq!(requests[1].0["user_id"], 456);
    assert!(requests.iter().all(|r| r.1 == "Bearer fixture-user"));
    drop(requests);
    assert!(client.send("kick:123", "blocked").await.is_err());
    server.abort();
}

#[tokio::test]
async fn channel_subscription_uses_app_token_and_does_not_silently_subscribe_own_channel() {
    let log: Arc<Mutex<Vec<Value>>> = Default::default();
    async fn channel() -> Json<Value> {
        Json(json!({"data":[{"slug":"same","broadcaster_user_id":123}]}))
    }
    async fn existing() -> Json<Value> {
        Json(json!({"data":[]}))
    }
    async fn subscribe(
        State(log): State<Arc<Mutex<Vec<Value>>>>,
        headers: axum::http::HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer fixture-app-token");
        log.lock().await.push(body.clone());
        Json(
            json!({"data":body["events"].as_array().unwrap().iter().map(|v|json!({"name":v["name"],"subscription_id":format!("sub-{}",v["name"].as_str().unwrap()),"version":1})).collect::<Vec<_>>()}),
        )
    }
    let app = Router::new()
        .route("/v1/channels", get(channel))
        .route("/v1/events/subscriptions", get(existing).post(subscribe))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (mut client, mut rx) = fixture();
    Arc::get_mut(&mut client).unwrap().api = format!("http://{addr}");
    client.state.lock().await.app = Some((
        "fixture-app-token".into(),
        Instant::now() + Duration::from_secs(3600),
    ));
    client.join("kick:same".into()).await.unwrap();
    assert_eq!(log.lock().await[0]["broadcaster_user_id"], 123);
    assert!(
        matches!(rx.try_recv().unwrap(),UiEvent::Connected{channel,channel_id} if channel=="kick:same"&&channel_id=="kick:123")
    );
    assert!(
        client
            .state
            .lock()
            .await
            .subscriptions
            .values()
            .all(|(c, _)| c == "kick:same")
    );
    client.part("kick:same").await;
    assert!(client.state.lock().await.subscriptions.is_empty());
    server.abort();
}

#[tokio::test]
async fn callback_requires_fresh_matching_state_before_network_or_vault() {
    let (client, _) = fixture();
    client.state.lock().await.pending = Some(Pending {
        state: "expected".into(),
        verifier: "private-verifier".into(),
        at: Instant::now(),
    });
    assert!(
        client
            .callback("code", "forged")
            .await
            .unwrap_err()
            .contains("stan")
    );
    assert!(client.state.lock().await.pending.is_some());
}
#[test]
fn native_emote_encoding_preserves_whitespace_and_does_not_change_other_words() {
    let map = HashMap::from([("KEKW".into(), "37226".into())]);
    assert_eq!(
        encode_emotes("Hi  KEKW\ttext", Some(&map)),
        "Hi  [emote:37226:KEKW]\ttext"
    );
}
