//! Sterowanie OBS przez wbudowany w OBS 28+ serwer WebSocket (obs-websocket v5).
//!
//! LimboAI zmienia sceny, wycisza mikrofon, pokazuje/chowa źródła, nagrywa,
//! zapisuje powtórkę, a silnik dowiaduje się o starcie i końcu streama
//! (→ podsumowanie streama).
//!
//! Włączenie (raz): OBS → Narzędzia → Ustawienia serwera WebSocket → „Włącz
//! serwer WebSocket” → „Pokaż dane połączenia” → hasło wklej w panelu LiMBOCHAT.
//! Hasło trafia do sejfu Windows (jak klucze API).
//!
//! Protokół: Hello (op 0) → Identify (op 1, z uwierzytelnieniem SHA-256) →
//! Identified (op 2); żądania op 6 → odpowiedzi op 7; zdarzenia op 5.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::jarvis::secrets::{self, Provider};

/// Zdarzenia: Scenes (4) + Outputs (64).
const EVENT_SUBSCRIPTIONS: u64 = 4 | 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ObsConfig {
    pub enabled: bool,
    pub port: u16,
}

impl Default for ObsConfig {
    fn default() -> Self {
        Self { enabled: true, port: 4455 }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ObsStatus {
    pub enabled: bool,
    pub connected: bool,
    pub has_password: bool,
    pub error: Option<String>,
    pub version: Option<String>,
    pub scenes: Vec<String>,
    pub current_scene: Option<String>,
    pub streaming: bool,
    pub recording: bool,
}

/// Zdarzenia dla huba.
#[derive(Debug)]
pub enum ObsEvent {
    /// Stream ruszył; `seconds` = jak długo już trwa (gdy połączyliśmy się w trakcie).
    StreamStarted { seconds: u64 },
    StreamStopped,
    Status(ObsStatus),
}

enum ObsCmd {
    Request { kind: String, data: Value, reply: oneshot::Sender<Result<Value, String>> },
    Configure(ObsConfig),
    Reconnect,
}

#[derive(Clone)]
pub struct ObsHandle {
    tx: mpsc::UnboundedSender<ObsCmd>,
    status: Arc<Mutex<ObsStatus>>,
    replays: tokio::sync::broadcast::Sender<String>,
}

impl ObsHandle {
    pub async fn capture_replay(&self)->Result<String,String>{
        let mut saved=self.replays.subscribe();
        let status=self.request("GetReplayBufferStatus",json!({})).await?;
        if status["outputActive"]!=true{return Err("Włącz bufor powtórek w OBS przed zapisywaniem powtórki.".into());}
        self.request("SaveReplayBuffer",json!({})).await?;
        tokio::time::timeout(Duration::from_secs(15),saved.recv()).await.map_err(|_|"OBS nie potwierdził zapisu powtórki. Sprawdź folder nagrań.".to_owned())?.map_err(|_|"OBS rozłączył zapis powtórki.".into())
    }
    pub fn status(&self) -> ObsStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn configure(&self, config: ObsConfig) {
        let _ = self.tx.send(ObsCmd::Configure(config));
    }

    /// Po zmianie hasła.
    pub fn reconnect(&self) {
        let _ = self.tx.send(ObsCmd::Reconnect);
    }

    /// Żądanie obs-websocket (np. `SetCurrentProgramScene`) → `responseData`.
    pub async fn request(&self, kind: &str, data: Value) -> Result<Value, String> {
        if !self.status().connected {
            return Err(not_connected_hint(&self.status()));
        }
        let (reply, rx) = oneshot::channel();
        self.tx.send(ObsCmd::Request { kind: kind.to_owned(), data, reply }).map_err(|_| "OBS: silnik się zatrzymuje".to_owned())?;
        tokio::time::timeout(Duration::from_secs(8), rx)
            .await
            .map_err(|_| "OBS nie odpowiedział.".to_owned())?
            .map_err(|_| "OBS rozłączony.".to_owned())?
    }
}

pub fn not_connected_hint(status: &ObsStatus) -> String {
    if !status.enabled {
        return "Sterowanie OBS jest wyłączone w ustawieniach LiMBOCHAT (⚙ → OBS).".into();
    }
    status.error.clone().unwrap_or_else(|| "Nie mam połączenia z OBS.".into())
}

pub fn spawn(config: ObsConfig, events: mpsc::UnboundedSender<ObsEvent>) -> ObsHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let status = Arc::new(Mutex::new(ObsStatus { enabled: config.enabled, ..Default::default() }));
    let (replays,_)=tokio::sync::broadcast::channel(8);
    tokio::spawn(run(config, rx, status.clone(), events,replays.clone()));
    ObsHandle { tx, status,replays }
}

fn url(port: u16) -> String {
    std::env::var("LIMBOCHAT_OBS_URL").unwrap_or_else(|_| format!("ws://127.0.0.1:{port}"))
}

async fn run(mut config: ObsConfig, mut rx: mpsc::UnboundedReceiver<ObsCmd>, status: Arc<Mutex<ObsStatus>>, events: mpsc::UnboundedSender<ObsEvent>, replays:tokio::sync::broadcast::Sender<String>) {
    let set = |f: &dyn Fn(&mut ObsStatus)| {
        let snapshot = {
            let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut s);
            s.clone()
        };
        let _ = events.send(ObsEvent::Status(snapshot));
    };
    loop {
        let has_password = secrets::load(Provider::Obs).await.is_some();
        let enabled = config.enabled;
        set(&|s| {
            s.enabled = enabled;
            s.has_password = has_password;
        });
        let retry = if config.enabled {
            match session(&config, &mut rx, &status, &events,&replays).await {
                Ok(SessionEnd::Reconfigure(c)) => {
                    config = c;
                    Duration::ZERO
                }
                Ok(SessionEnd::Reconnect) => Duration::ZERO,
                Err((message, wait)) => {
                    let msg = message.clone();
                    set(&|s| {
                        s.connected = false;
                        s.error = Some(msg.clone());
                        s.streaming = false;
                        s.recording = false;
                    });
                    wait
                }
            }
        } else {
            set(&|s| {
                s.connected = false;
                s.error = None;
            });
            Duration::from_secs(3600)
        };
        // Czekamy na ponowną próbę albo zmianę ustawień.
        let deadline = tokio::time::sleep(retry);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                cmd = rx.recv() => match cmd {
                    None => return,
                    Some(ObsCmd::Configure(c)) => { config = c; break; }
                    Some(ObsCmd::Reconnect) => break,
                    Some(ObsCmd::Request { reply, .. }) => {
                        let _ = reply.send(Err(not_connected_hint(&status.lock().unwrap_or_else(|e| e.into_inner()).clone())));
                    }
                }
            }
        }
    }
}

enum SessionEnd {
    Reconfigure(ObsConfig),
    Reconnect,
}

type Fail = (String, Duration);

async fn session(
    config: &ObsConfig,
    rx: &mut mpsc::UnboundedReceiver<ObsCmd>,
    status: &Arc<Mutex<ObsStatus>>,
    events: &mpsc::UnboundedSender<ObsEvent>,
    replays: &tokio::sync::broadcast::Sender<String>,
) -> Result<SessionEnd, Fail> {
    let wait = Duration::from_secs(10);
    let (ws, _) = tokio_tungstenite::connect_async(url(config.port)).await.map_err(|_| {
        (
            "OBS WebSocket wyłączony albo OBS zamknięty — w OBS: Narzędzia → Ustawienia serwera WebSocket → Włącz.".to_owned(),
            wait,
        )
    })?;
    let (mut sink, mut stream) = ws.split();

    // Hello → Identify (z hasłem, jeśli serwer go wymaga).
    let hello = tokio::time::timeout(Duration::from_secs(5), next_json(&mut stream))
        .await
        .map_err(|_| ("OBS nie przywitał się.".to_owned(), wait))?
        .map_err(|e| (e, wait))?;
    let mut identify = json!({ "rpcVersion": 1, "eventSubscriptions": EVENT_SUBSCRIPTIONS });
    if let Some(auth) = hello["d"].get("authentication") {
        let password = secrets::load(Provider::Obs)
            .await
            .ok_or(("OBS wymaga hasła — wklej je w ⚙ → OBS (OBS: Narzędzia → Ustawienia serwera WebSocket → Pokaż dane połączenia).".to_owned(), Duration::from_secs(3600)))?;
        identify["authentication"] = json!(auth_string(&password, auth["salt"].as_str().unwrap_or_default(), auth["challenge"].as_str().unwrap_or_default()));
    }
    send(&mut sink, 1, identify).await.map_err(|e| (e, wait))?;
    let identified = tokio::time::timeout(Duration::from_secs(5), next_json(&mut stream)).await.map_err(|_| ("OBS nie odpowiedział.".to_owned(), wait))?;
    match identified {
        Ok(v) if v["op"] == 2 => {}
        // Zamknięcie 4009 = złe hasło.
        _ => return Err(("Złe hasło OBS WebSocket — sprawdź je w ⚙ → OBS.".into(), Duration::from_secs(3600))),
    }
    let version = hello["d"]["obsWebSocketVersion"].as_str().map(str::to_owned);

    // Stan początkowy: sceny, stream, nagrywanie.
    let mut pending: HashMap<String, oneshot::Sender<Result<Value, String>>> = HashMap::new();
    let mut counter = 0u64;
    let mut ask = |kind: &str| {
        counter += 1;
        (format!("init-{counter}"), json!({ "requestType": kind, "requestId": format!("init-{counter}") }))
    };
    let (scenes_id, scenes_req) = ask("GetSceneList");
    let (stream_id, stream_req) = ask("GetStreamStatus");
    let (record_id, record_req) = ask("GetRecordStatus");
    for req in [scenes_req, stream_req, record_req] {
        send(&mut sink, 6, req).await.map_err(|e| (e, wait))?;
    }
    {
        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
        s.connected = true;
        s.error = None;
        s.version = version;
        let _ = events.send(ObsEvent::Status(s.clone()));
    }
    tracing::info!("OBS WebSocket połączony");

    let mut next_id = 0u64;
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None => return Ok(SessionEnd::Reconnect),
                Some(ObsCmd::Configure(c)) => return Ok(SessionEnd::Reconfigure(c)),
                Some(ObsCmd::Reconnect) => return Ok(SessionEnd::Reconnect),
                Some(ObsCmd::Request { kind, data, reply }) => {
                    next_id += 1;
                    let id = format!("r{next_id}");
                    let req = json!({ "requestType": kind, "requestId": id, "requestData": data });
                    if let Err(e) = send(&mut sink, 6, req).await {
                        let _ = reply.send(Err(e.clone()));
                        return Err((e, wait));
                    }
                    pending.insert(id, reply);
                }
            },
            msg = next_json(&mut stream) => {
                let msg = msg.map_err(|_| ("OBS zamknięty albo rozłączony — połączę się ponownie.".to_owned(), wait))?;
                let d = &msg["d"];
                match msg["op"].as_u64() {
                    Some(7) => {
                        let id = d["requestId"].as_str().unwrap_or_default();
                        let ok = d["requestStatus"]["result"].as_bool().unwrap_or(false);
                        let data = d.get("responseData").cloned().unwrap_or(Value::Null);
                        if id == scenes_id {
                            apply(status, events, |s| {
                                s.scenes = scene_names(&data);
                                s.current_scene = data["currentProgramSceneName"].as_str().map(str::to_owned);
                            });
                        } else if id == stream_id {
                            let active = data["outputActive"].as_bool().unwrap_or(false);
                            apply(status, events, |s| s.streaming = active);
                            if active {
                                let seconds = data["outputDuration"].as_u64().unwrap_or(0) / 1000;
                                let _ = events.send(ObsEvent::StreamStarted { seconds });
                            }
                        } else if id == record_id {
                            let active = data["outputActive"].as_bool().unwrap_or(false);
                            apply(status, events, |s| s.recording = active);
                        } else if let Some(reply) = pending.remove(id) {
                            let result = if ok {
                                Ok(data)
                            } else {
                                Err(request_error(d["requestStatus"]["code"].as_u64().unwrap_or(0), d["requestStatus"]["comment"].as_str().unwrap_or_default()))
                            };
                            let _ = reply.send(result);
                        }
                    }
                    Some(5) => {
                        if d["eventType"]=="ReplayBufferSaved" { if let Some(path)=d["eventData"]["savedReplayPath"].as_str().filter(|p|!p.is_empty()){let _=replays.send(path.into());} }
                        on_event(d, status, events);
                    },
                    _ => {}
                }
            }
        }
    }
}

fn on_event(d: &Value, status: &Arc<Mutex<ObsStatus>>, events: &mpsc::UnboundedSender<ObsEvent>) {
    let data = &d["eventData"];
    match d["eventType"].as_str().unwrap_or_default() {
        "StreamStateChanged" => match data["outputState"].as_str().unwrap_or_default() {
            "OBS_WEBSOCKET_OUTPUT_STARTED" => {
                apply(status, events, |s| s.streaming = true);
                let _ = events.send(ObsEvent::StreamStarted { seconds: 0 });
            }
            "OBS_WEBSOCKET_OUTPUT_STOPPED" => {
                apply(status, events, |s| s.streaming = false);
                let _ = events.send(ObsEvent::StreamStopped);
            }
            _ => {}
        },
        "RecordStateChanged" => {
            let active = data["outputActive"].as_bool().unwrap_or(false);
            apply(status, events, |s| s.recording = active);
        }
        "CurrentProgramSceneChanged" => {
            let name = data["sceneName"].as_str().map(str::to_owned);
            apply(status, events, |s| s.current_scene = name.clone());
        }
        "SceneListChanged" => {
            let names = scene_names(data);
            apply(status, events, |s| s.scenes = names.clone());
        }
        "SceneNameChanged" => {
            let (old, new) = (data["oldSceneName"].as_str().unwrap_or_default().to_owned(), data["sceneName"].as_str().unwrap_or_default().to_owned());
            apply(status, events, |s| {
                for n in &mut s.scenes {
                    if *n == old {
                        *n = new.clone();
                    }
                }
            });
        }
        _ => {}
    }
}

fn apply(status: &Arc<Mutex<ObsStatus>>, events: &mpsc::UnboundedSender<ObsEvent>, f: impl FnOnce(&mut ObsStatus)) {
    let snapshot = {
        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut s);
        s.clone()
    };
    let _ = events.send(ObsEvent::Status(snapshot));
}

/// Sceny w kolejności z OBS (lista przychodzi od dołu).
fn scene_names(data: &Value) -> Vec<String> {
    let mut list: Vec<(i64, String)> = data["scenes"]
        .as_array()
        .map(|a| a.iter().filter_map(|s| Some((s["sceneIndex"].as_i64().unwrap_or(0), s["sceneName"].as_str()?.to_owned()))).collect())
        .unwrap_or_default();
    list.sort_by_key(|(i, _)| -i);
    list.into_iter().map(|(_, n)| n).collect()
}

fn request_error(code: u64, comment: &str) -> String {
    match code {
        600 => "Nie ma takiego źródła ani sceny w OBS.".into(),
        500 => "To już jest włączone/wyłączone w OBS.".into(),
        501 => "Tego nie da się teraz zrobić w OBS (np. nagrywanie nie trwa).".into(),
        604 => "Bufor powtórek w OBS jest wyłączony — włącz go w OBS (Ustawienia → Wyjście → Bufor powtórek).".into(),
        _ if comment.is_empty() => format!("OBS odrzucił polecenie (kod {code})."),
        _ => format!("OBS: {comment}"),
    }
}

/// Uwierzytelnianie obs-websocket: base64(sha256(base64(sha256(hasło + sól)) + wyzwanie)).
pub fn auth_string(password: &str, salt: &str, challenge: &str) -> String {
    let secret = B64.encode(Sha256::digest(format!("{password}{salt}").as_bytes()));
    B64.encode(Sha256::digest(format!("{secret}{challenge}").as_bytes()))
}

async fn send<S>(sink: &mut S, op: u8, d: Value) -> Result<(), String>
where
    S: futures_util::Sink<Message> + Unpin,
{
    sink.send(Message::Text(json!({ "op": op, "d": d }).to_string().into())).await.map_err(|_| "OBS rozłączony.".to_owned())
}

async fn next_json<S>(stream: &mut S) -> Result<Value, String>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(msg) = stream.next().await {
        match msg.map_err(|e| e.to_string())? {
            Message::Text(t) => return serde_json::from_str(t.as_str()).map_err(|e| e.to_string()),
            Message::Close(frame) => return Err(frame.map(|f| f.code.to_string()).unwrap_or_else(|| "zamknięte".into())),
            _ => continue,
        }
    }
    Err("zamknięte".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture()->(ObsHandle,mpsc::UnboundedReceiver<ObsCmd>){let(tx,rx)=mpsc::unbounded_channel();let(replays,_)=tokio::sync::broadcast::channel(8);(ObsHandle{tx,replays,status:Arc::new(Mutex::new(ObsStatus{connected:true,current_scene:Some("Gra".into()),..Default::default()}))},rx)}

    #[tokio::test]
    async fn replay_save_waits_for_obs_file_event_and_rejects_disabled_buffer(){
        let(handle,mut rx)=fixture();let replays=handle.replays.clone();let job=tokio::spawn(async move{handle.capture_replay().await});
        let ObsCmd::Request{kind,reply,..}=rx.recv().await.unwrap()else{panic!()};assert_eq!(kind,"GetReplayBufferStatus");let _=reply.send(Ok(json!({"outputActive":true})));
        let ObsCmd::Request{kind,reply,..}=rx.recv().await.unwrap()else{panic!()};assert_eq!(kind,"SaveReplayBuffer");let _=reply.send(Ok(json!({})));assert!(!job.is_finished());replays.send("C:\\Clips\\now.mkv".into()).unwrap();assert_eq!(job.await.unwrap().unwrap(),"C:\\Clips\\now.mkv");
        let(handle,mut rx)=fixture();let job=tokio::spawn(async move{handle.capture_replay().await});let ObsCmd::Request{reply,..}=rx.recv().await.unwrap()else{panic!()};let _=reply.send(Ok(json!({"outputActive":false})));assert!(job.await.unwrap().is_err());assert!(rx.try_recv().is_err());
    }
    #[tokio::test]
    async fn replay_playback_restores_original_source_after_end(){
        let(handle,mut rx)=fixture();let(_stop,stop_rx)=tokio::sync::watch::channel(false);
        let job=tokio::spawn(crate::studio::play_replay(handle,"Powtórka".into(),"C:\\Clips\\new.mkv".into(),stop_rx));
        let mut calls=Vec::new();let mut input=json!({"local_file":"C:\\old.mp4","looping":true});let original=input.clone();
        loop{tokio::select!{
            command=rx.recv()=>{let Some(ObsCmd::Request{kind,data,reply})=command else{break};calls.push((kind.clone(),data.clone()));let result=match kind.as_str(){"GetInputSettings"=>json!({"inputKind":"ffmpeg_source","inputSettings":input}),"GetSceneItemId"=>json!({"sceneItemId":7}),"GetSceneItemEnabled"=>json!({"sceneItemEnabled":false}),"GetMediaInputStatus"=>json!({"mediaState":"OBS_MEDIA_STATE_ENDED"}),"SetInputSettings"=>{input=data["inputSettings"].clone();json!({})},_=>json!({})};let _=reply.send(Ok(result));}
            _=tokio::time::sleep(Duration::from_secs(3))=>panic!("powtórka nie zakończyła się"),
        }}
        assert!(job.await.unwrap().is_ok());assert_eq!(input,original);assert!(calls.iter().any(|(kind,data)|kind=="SetSceneItemEnabled"&&data["sceneItemEnabled"]==false));assert_eq!(calls.last().unwrap().0,"SetInputSettings");
    }

    /// Przykład z dokumentacji obs-websocket 5.
    #[test]
    fn authentication_matches_protocol_docs() {
        assert_eq!(
            auth_string("supersecretpassword", "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=", "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY="),
            "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4="
        );
    }

    #[test]
    fn scenes_in_obs_order() {
        let data = json!({ "scenes": [{ "sceneIndex": 0, "sceneName": "Przerwa" }, { "sceneIndex": 1, "sceneName": "Gra" }, { "sceneIndex": 2, "sceneName": "Start" }] });
        assert_eq!(scene_names(&data), ["Start", "Gra", "Przerwa"]);
    }
}
