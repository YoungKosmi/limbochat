//! Proces silnika: start, pilnowanie OBS, zatrzymanie i przekazanie pracy
//! nowej wersji przy aktualizacji.
//!
//! Argumenty:
//! * `--obs-pid N` — wyłącz się, gdy OBS o tym numerze procesu się zamknie,
//! * `--takeover --updated-from X` — start po aktualizacji: przejmij port
//!   (poprzednia wersja właśnie go zwalnia) i czat z `handover.json`,
//! * `--selftest`, `--version` — wypisz wersję i zakończ,
//! * `--keygen`, `--pubkey`, `--sign` — narzędzia do wydawania aktualizacji.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use tokio::{net::TcpListener, sync::mpsc};

use crate::{
    PORT, auth, config,
    hub::{EngineLink, Handover, Hub},
    jarvis, server, update,
};

/// Polecenia dla pętli głównej silnika.
pub enum Control {
    /// Zakończ pracę (OBS zamknięty albo polecenie z wtyczki).
    Exit(&'static str),
    /// Oddaj pracę nowej wersji.
    Handover(update::Staged),
}

/// Stan współdzielony z serwerem (endpointy /control/*).
#[derive(Clone)]
pub struct ControlState {
    pub token: Arc<str>,
    pub obs_pid: Arc<AtomicU32>,
    pub tx: mpsc::UnboundedSender<Control>,
}

#[derive(Default)]
struct Args {
    obs_pid: u32,
    takeover: bool,
    updated_from: Option<String>,
}

/// Folder silnika: `%LOCALAPPDATA%\LiMBOCHAT\engine` (zapisywalny bez uprawnień
/// administratora — dzięki temu aktualizacje nie pytają o zgodę).
pub fn engine_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LIMBOCHAT_ENGINE_DIR") {
        return PathBuf::from(dir);
    }
    directories::BaseDirs::new()
        .map(|b| b.data_local_dir().join("LiMBOCHAT").join("engine"))
        .unwrap_or_else(|| std::env::temp_dir().join("LiMBOCHAT-engine"))
}

pub fn main() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| argv.get(i).map(String::as_str).unwrap_or_default();
    match arg(0) {
        "--version" | "--selftest" => {
            println!("limbochat-engine {}", crate::version());
            return 0;
        }
        "--keygen" => return update::cli_keygen(Path::new(arg(1))),
        "--pubkey" => return update::cli_pubkey(Path::new(arg(1))),
        "--sign" => {
            return update::cli_sign(Path::new(arg(1)), arg(2), arg(3), Path::new(arg(4)), Path::new(arg(5)));
        }
        _ => {}
    }
    let mut args = Args::default();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--obs-pid" => {
                args.obs_pid = arg(i + 1).parse().unwrap_or(0);
                i += 1;
            }
            "--takeover" => args.takeover = true,
            "--updated-from" => {
                args.updated_from = Some(arg(i + 1).to_owned()).filter(|v| !v.is_empty());
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    init_logging();
    tracing::info!("silnik LiMBOCHAT {} startuje (OBS pid {}, przejęcie: {})", crate::version(), args.obs_pid, args.takeover);
    auth::set_keyring_account("twitch-oauth-token-obs");
    // LimboAI steruje streamem — potrzebuje dodatkowych uprawnień Twitch.
    auth::set_extra_scopes(jarvis::twitch::EXTRA_SCOPES);

    let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(3).thread_name("limbochat").enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("nie udało się uruchomić runtime: {e}");
            return 1;
        }
    };
    let code = runtime.block_on(run(args));
    runtime.shutdown_timeout(Duration::from_millis(800));
    tracing::info!("silnik LiMBOCHAT {} zakończył pracę", crate::version());
    code
}

async fn run(args: Args) -> i32 {
    let dir = engine_dir();
    let _ = std::fs::create_dir_all(&dir);
    let obs_pid = Arc::new(AtomicU32::new(args.obs_pid));

    let Some(listener) = acquire_port(&dir, &args).await else { return 0 };
    let token: Arc<str> = random_token().into();
    write_private(&dir.join("control.token"), &token);
    update::cleanup(&dir);

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    spawn_watchdog(obs_pid.clone(), control_tx.clone());
    let control = ControlState { token, obs_pid: obs_pid.clone(), tx: control_tx.clone() };

    let mut listener = Some(listener);
    let mut handover = if args.takeover { take_handover(&dir) } else { None };
    let mut updated_from = args.updated_from;

    loop {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (updater, updater_rx) = update::channel(updated_from.take());
        let link = EngineLink { updater: Some(updater.clone()), handover: handover.take() };
        let hub = Hub::spawn(&tokio::runtime::Handle::current(), shutdown_rx.clone(), link);
        let Some(bound) = listener.take() else { return 1 };
        let server = tokio::spawn(server::serve(bound, hub.clone(), shutdown_rx.clone(), control.clone()));
        let updates = tokio::spawn(update::run(updater, updater_rx, hub.clone(), control_tx.clone(), dir.clone()));
        tracing::info!("panel: http://127.0.0.1:{PORT}  ·  overlay: http://127.0.0.1:{PORT}/overlay");

        let command = control_rx.recv().await;
        updates.abort();
        match command {
            None | Some(Control::Exit(_)) => {
                if let Some(Control::Exit(reason)) = command {
                    tracing::info!("zatrzymanie: {reason}");
                }
                let _ = shutdown_tx.send(true);
                stop_server(server, Duration::from_secs(3)).await;
                return 0;
            }
            Some(Control::Handover(staged)) => {
                // 1. Czat dla nowej wersji, 2. zwolnienie portu, 3. start nowej wersji.
                let data = hub.export().await;
                save_handover(&dir, &data);
                let _ = shutdown_tx.send(true);
                stop_server(server, Duration::from_secs(4)).await;
                tokio::time::sleep(Duration::from_millis(300)).await;

                let previous = update::current(&dir);
                let started = update::set_current(&dir, &staged.exe).is_ok()
                    && spawn_takeover(&staged.exe, obs_pid.load(Ordering::Relaxed)).is_ok();
                if started && wait_for_version(&staged.version, Duration::from_secs(12)).await {
                    tracing::info!("wersja {} przejęła pracę — kończę", staged.version);
                    return 0;
                }

                // Nowa wersja nie wstała: wracamy do pracy na starej.
                tracing::error!("wersja {} nie wystartowała — zostaję na {}", staged.version, crate::version());
                match previous {
                    Some(name) => {
                        let _ = update::set_current(&dir, &dir.join(name));
                    }
                    None => {
                        let _ = std::fs::remove_file(dir.join("current.txt"));
                    }
                }
                let _ = std::fs::remove_file(&staged.exe);
                update::skip_version(&dir, &staged.version);
                handover = take_handover(&dir);
                listener = bind_with_retry(Duration::from_secs(15)).await;
                if listener.is_none() {
                    return 1;
                }
            }
        }
    }
}

/// Czeka na łagodne zamknięcie serwera, a potem i tak go przerywa —
/// port musi być wolny dla nowej wersji.
async fn stop_server(mut server: tokio::task::JoinHandle<()>, grace: Duration) {
    if tokio::time::timeout(grace, &mut server).await.is_err() {
        server.abort();
        let _ = server.await;
    }
}

/// Zajmuje port panelu. Jeśli działa już inny silnik LiMBOCHAT (np. po
/// ponownym uruchomieniu OBS), przekazuje mu numer nowego OBS i kończy.
async fn acquire_port(dir: &Path, args: &Args) -> Option<TcpListener> {
    if args.takeover {
        // Poprzednia wersja właśnie zwalnia port.
        return bind_with_retry(Duration::from_secs(20)).await;
    }
    for attempt in 0..15 {
        if let Ok(listener) = bind().await {
            return Some(listener);
        }
        if let Some(version) = running_engine_version().await {
            let token = std::fs::read_to_string(dir.join("control.token")).unwrap_or_default();
            let ok = control_post(&format!("/control/adopt?pid={}", args.obs_pid), token.trim()).await;
            tracing::info!("silnik {version} już działa — przekazano mu OBS (ok: {ok}), kończę");
            return None;
        }
        if attempt == 0 {
            tracing::warn!("port {PORT} zajęty przez inny program — czekam");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    tracing::error!("port {PORT} wciąż zajęty — panel nie wystartuje");
    None
}

async fn bind() -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], PORT))).await
}

async fn bind_with_retry(total: Duration) -> Option<TcpListener> {
    let deadline = tokio::time::Instant::now() + total;
    loop {
        match bind().await {
            Ok(l) => return Some(l),
            Err(e) if tokio::time::Instant::now() >= deadline => {
                tracing::error!("nie można zająć portu {PORT}: {e}");
                return None;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(150)).await,
        }
    }
}

fn local_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build().unwrap_or_default()
}

/// Wersja silnika, który odpowiada na porcie panelu (jeśli to LiMBOCHAT).
async fn running_engine_version() -> Option<String> {
    let resp = local_client().get(format!("http://127.0.0.1:{PORT}/control/health")).send().await.ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    (v["app"] == "limbochat").then(|| v["version"].as_str().unwrap_or_default().to_owned())
}

async fn control_post(path: &str, token: &str) -> bool {
    local_client()
        .post(format!("http://127.0.0.1:{PORT}{path}"))
        .header("X-LiMBOCHAT-Token", token)
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

async fn wait_for_version(version: &str, total: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + total;
    while tokio::time::Instant::now() < deadline {
        if running_engine_version().await.as_deref() == Some(version) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    false
}

fn spawn_takeover(exe: &Path, obs_pid: u32) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["--takeover", "--obs-pid", &obs_pid.to_string(), "--updated-from", crate::version()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Bez okna i niezależnie od nas (my zaraz się zamkniemy).
        cmd.creation_flags(0x0800_0000 | 0x0000_0008 | 0x0000_0200); // NO_WINDOW | DETACHED | NEW_PROCESS_GROUP
    }
    cmd.spawn().map(|_| ())
}

// ── Przekazanie czatu ──────────────────────────────────────────────────

fn save_handover(dir: &Path, data: &Handover) {
    if let Ok(json) = serde_json::to_vec(data) {
        write_private(&dir.join("handover.json"), &String::from_utf8_lossy(&json));
    }
}

fn take_handover(dir: &Path) -> Option<Handover> {
    let path = dir.join("handover.json");
    let bytes = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_slice(&bytes).ok()
}

// ── Pilnowanie OBS ─────────────────────────────────────────────────────

/// Gdy OBS się zamknie (także po awarii), silnik kończy pracę.
fn spawn_watchdog(pid: Arc<AtomicU32>, tx: mpsc::UnboundedSender<Control>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let current = pid.load(Ordering::Relaxed);
            if current != 0 && !process_alive(current) {
                let _ = tx.send(Control::Exit("OBS został zamknięty"));
                return;
            }
        }
    });
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    // SAFETY: uchwyt tylko do oczekiwania na zakończenie; zamykany od razu.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return false;
        }
        let alive = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        CloseHandle(handle);
        alive
    }
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

// ── Drobne ─────────────────────────────────────────────────────────────

fn random_token() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 24];
    rand_core::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Zapis pliku czytelnego tylko dla użytkownika (token, czat).
fn write_private(path: &Path, text: &str) {
    let _ = std::fs::write(path, text);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

/// Log: %APPDATA%\LiMBOCHAT\engine.log (dopisywany; czyszczony powyżej 2 MB).
fn init_logging() {
    let Some(dir) = config::data_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("engine.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info,twitch_irc=warn".to_owned());
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .try_init();
}
