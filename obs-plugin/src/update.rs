//! Autoaktualizacje silnika z GitHub Releases — bez restartu OBS.
//!
//! ## Przebieg
//! 1. Co 20 minut (i na żądanie z panelu) pytamy GitHub o najnowsze wydanie
//!    repozytorium wpisanego przy budowaniu (`LIMBOCHAT_UPDATE_REPO`).
//! 2. Jeśli jest nowsza wersja, pobieramy plik silnika i jego podpis `.sig`.
//! 3. **Podpis Ed25519** musi pasować do klucza publicznego wbudowanego
//!    w silnik (`LIMBOCHAT_UPDATE_PUBKEY`). Podpis obejmuje wersję, nazwę
//!    pliku i SHA-256 — nie da się podsunąć starej ani obcej wersji, nawet
//!    gdyby ktoś przejął konto GitHub (bez prywatnego klucza autora).
//! 4. Nowy plik musi się uruchomić (`--selftest`).
//! 5. Gdy LimboAI nic nie robi (tryb `auto`) albo po kliknięciu w panelu,
//!    silnik przekazuje czat nowej wersji i oddaje jej port (moduł `engine`).
//!    Jeśli nowa wersja nie wstanie — stara wraca do pracy.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::{engine::Control, hub::HubHandle};

const CHECK_EVERY: Duration = Duration::from_secs(20 * 60);
const FIRST_CHECK: Duration = Duration::from_secs(15);
const MAX_ENGINE_SIZE: usize = 120 * 1024 * 1024;
const SIG_HEADER: &str = "limbochat-update-v1";

/// Nazwa pliku silnika w wydaniu na GitHubie dla tej platformy.
pub const ASSET: &str = if cfg!(windows) { "limbochat-engine-windows-x64.exe" } else { "limbochat-engine-linux-x64" };
const EXE_SUFFIX: &str = if cfg!(windows) { ".exe" } else { "" };

/// Repozytorium z wydaniami, np. `kosmi/limbochat` (wpisywane przy budowaniu).
pub fn repo() -> Option<String> {
    std::env::var("LIMBOCHAT_UPDATE_REPO")
        .ok()
        .or_else(|| option_env!("LIMBOCHAT_UPDATE_REPO").map(str::to_owned))
        .map(|r| r.trim().to_owned())
        .filter(|r| r.split('/').count() == 2 && !r.contains(' '))
}

fn public_key() -> Option<VerifyingKey> {
    let text = std::env::var("LIMBOCHAT_UPDATE_PUBKEY").ok().or_else(|| option_env!("LIMBOCHAT_UPDATE_PUBKEY").map(str::to_owned))?;
    let bytes: [u8; 32] = B64.decode(text.trim()).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

fn api_base() -> String {
    std::env::var("LIMBOCHAT_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".to_owned())
}

// ── Stan widoczny w panelu ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateStatus {
    pub version: String,
    /// Czy ta wersja w ogóle umie się aktualizować (repo + klucz wbudowane).
    pub enabled: bool,
    pub repo: Option<String>,
    /// disabled / idle / checking / up_to_date / downloading / ready / applying / error
    pub state: String,
    pub available: Option<String>,
    pub notes: Option<String>,
    pub error: Option<String>,
    /// Kiedy ostatnio sprawdzono (ms od 1970).
    pub checked_at: Option<i64>,
    /// Ustawione zaraz po aktualizacji: z jakiej wersji przyszliśmy.
    pub updated_from: Option<String>,
}

pub enum UpdateCmd {
    Check,
    Apply,
}

#[derive(Clone)]
pub struct UpdaterHandle {
    tx: mpsc::UnboundedSender<UpdateCmd>,
    status: Arc<Mutex<UpdateStatus>>,
}

impl UpdaterHandle {
    pub fn check(&self) {
        let _ = self.tx.send(UpdateCmd::Check);
    }

    pub fn apply(&self) {
        let _ = self.tx.send(UpdateCmd::Apply);
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

pub fn channel(updated_from: Option<String>) -> (UpdaterHandle, mpsc::UnboundedReceiver<UpdateCmd>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let enabled = repo().is_some() && public_key().is_some();
    let status = UpdateStatus {
        version: crate::version().to_owned(),
        enabled,
        repo: repo(),
        state: if enabled { "idle" } else { "disabled" }.to_owned(),
        updated_from,
        ..Default::default()
    };
    (UpdaterHandle { tx, status: Arc::new(Mutex::new(status)) }, rx)
}

/// Zmienia stan i rozsyła go do paneli.
struct Reporter {
    status: Arc<Mutex<UpdateStatus>>,
    hub: HubHandle,
}

impl Reporter {
    fn set(&self, f: impl FnOnce(&mut UpdateStatus)) {
        let snapshot = {
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut s);
            s.clone()
        };
        self.hub.update_status(snapshot);
    }
}

/// Pobrana i sprawdzona nowa wersja, gotowa do przejęcia pracy.
#[derive(Debug, Clone)]
pub struct Staged {
    pub version: String,
    pub exe: PathBuf,
}

/// Pętla aktualizacji. Kończy się, gdy silnik się zatrzymuje (kanał zamknięty).
pub async fn run(
    handle: UpdaterHandle,
    mut rx: mpsc::UnboundedReceiver<UpdateCmd>,
    hub: HubHandle,
    control: mpsc::UnboundedSender<Control>,
    dir: PathBuf,
) {
    let set = Reporter { status: handle.status.clone(), hub: hub.clone() };
    if !handle.status().enabled {
        // Nic do roboty, ale odbieramy polecenia (żeby panel nie wisiał).
        while rx.recv().await.is_some() {}
        return;
    }
    let http = reqwest::Client::builder()
        .user_agent(format!("LiMBOCHAT/{}", crate::version()))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap_or_default();

    let mut staged: Option<Staged> = None;
    let mut next_check = tokio::time::Instant::now() + FIRST_CHECK;
    let mut idle_poll = tokio::time::interval(Duration::from_secs(5));
    loop {
        let mut apply_now = false;
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None => return,
                Some(UpdateCmd::Check) => {
                    staged = check(&http, &dir, staged.take(), &set).await;
                    next_check = tokio::time::Instant::now() + CHECK_EVERY;
                }
                Some(UpdateCmd::Apply) => {
                    if staged.is_none() {
                        staged = check(&http, &dir, None, &set).await;
                    }
                    apply_now = staged.is_some();
                }
            },
            _ = tokio::time::sleep_until(next_check) => {
                staged = check(&http, &dir, staged.take(), &set).await;
                next_check = tokio::time::Instant::now() + CHECK_EVERY;
            }
            _ = idle_poll.tick(), if staged.is_some() => {
                let (auto, idle) = hub.update_gate().await;
                apply_now = auto && idle;
            }
        }
        if apply_now {
            if let Some(update) = staged.take() {
                set.set(|s| s.state = "applying".into());
                tracing::info!("aktualizacja: przekazuję pracę wersji {}", update.version);
                let _ = control.send(Control::Handover(update));
                // Silnik zatrzyma hub; jeśli nowa wersja nie wstanie, wystartuje nas od nowa.
                return;
            }
        }
    }
}

/// Sprawdza GitHuba; zwraca gotową (pobraną i sprawdzoną) aktualizację.
async fn check(
    http: &reqwest::Client,
    dir: &Path,
    staged: Option<Staged>,
    set: &Reporter,
) -> Option<Staged> {
    set.set(|s| {
        s.state = "checking".into();
        s.error = None;
    });
    match fetch(http, dir, staged, set).await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            set.set(|s| {
                s.state = "ready".into();
                s.available = Some(version.clone());
                s.checked_at = Some(chrono::Utc::now().timestamp_millis());
            });
            Some(update)
        }
        Ok(None) => {
            set.set(|s| {
                s.state = "up_to_date".into();
                s.available = None;
                s.checked_at = Some(chrono::Utc::now().timestamp_millis());
            });
            None
        }
        Err(message) => {
            tracing::warn!("aktualizacja: {message}");
            set.set(|s| {
                s.state = "error".into();
                s.error = Some(message.clone());
                s.checked_at = Some(chrono::Utc::now().timestamp_millis());
            });
            None
        }
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

async fn fetch(
    http: &reqwest::Client,
    dir: &Path,
    staged: Option<Staged>,
    set: &Reporter,
) -> Result<Option<Staged>, String> {
    let repo = repo().ok_or("brak repozytorium aktualizacji")?;
    let key = public_key().ok_or("brak klucza publicznego aktualizacji")?;
    let url = format!("{}/repos/{repo}/releases/latest", api_base());
    let resp = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "Brak połączenia z GitHubem.".to_owned())?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Err(format!("Repozytorium {repo} nie ma jeszcze wydań (albo jest prywatne).")),
        403 | 429 => return Err("GitHub chwilowo ogranicza zapytania — spróbuję później.".into()),
        code => return Err(format!("GitHub odpowiedział błędem {code}.")),
    }
    let release: Release = resp.json().await.map_err(|_| "Niezrozumiała odpowiedź GitHuba.".to_owned())?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let version = release.tag_name.trim_start_matches(['v', 'V']).to_owned();
    if !is_newer(&version, crate::version()) {
        return Ok(None);
    }
    if skipped(dir).as_deref() == Some(version.as_str()) {
        return Err(format!("Wersja {version} nie uruchomiła się na tym komputerze — zostaję na {} i czekam na następną.", crate::version()));
    }
    if let Some(s) = staged.filter(|s| s.version == version && s.exe.exists()) {
        return Ok(Some(s));
    }
    let notes: String = release.body.unwrap_or_default().chars().take(600).collect();
    let v = version.clone();
    set.set(|s| {
        s.state = "downloading".into();
        s.available = Some(v.clone());
        s.notes = Some(notes.clone()).filter(|n| !n.trim().is_empty());
    });

    let asset = release.assets.iter().find(|a| a.name == ASSET).ok_or_else(|| format!("Wydanie {version} nie ma pliku {ASSET}."))?;
    let sig_name = format!("{ASSET}.sig");
    let sig_asset = release.assets.iter().find(|a| a.name == sig_name).ok_or_else(|| format!("Wydanie {version} nie ma podpisu {sig_name}."))?;
    if asset.size as usize > MAX_ENGINE_SIZE {
        return Err("Plik aktualizacji jest podejrzanie duży — pominięto.".into());
    }
    let sig = download(http, &sig_asset.browser_download_url, 8 * 1024).await?;
    let sig = String::from_utf8(sig).map_err(|_| "Zły format podpisu.".to_owned())?;
    let bytes = download(http, &asset.browser_download_url, MAX_ENGINE_SIZE).await?;
    verify(&sig, &key, &version, ASSET, &bytes)?;

    // Zapis obok obecnej wersji (bez nadpisywania działającego pliku).
    std::fs::create_dir_all(dir).map_err(|e| format!("Nie mogę utworzyć folderu silnika: {e}"))?;
    let exe = dir.join(format!("limbochat-engine-{version}{EXE_SUFFIX}"));
    let tmp = dir.join(format!("limbochat-engine-{version}.download"));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("Nie mogę zapisać aktualizacji: {e}"))?;
    make_executable(&tmp);
    std::fs::rename(&tmp, &exe).map_err(|e| format!("Nie mogę zapisać aktualizacji: {e}"))?;

    selftest(&exe, &version).await?;
    tracing::info!("aktualizacja {version} pobrana i sprawdzona");
    Ok(Some(Staged { version, exe }))
}

async fn download(http: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut resp = http
        .get(url)
        .header("Accept", "application/octet-stream")
        .send()
        .await
        .map_err(|_| "Nie udało się pobrać aktualizacji.".to_owned())?;
    if !resp.status().is_success() {
        return Err(format!("Pobieranie aktualizacji: błąd {}.", resp.status().as_u16()));
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|_| "Pobieranie przerwane.".to_owned())? {
        out.extend_from_slice(&chunk);
        if out.len() > limit {
            return Err("Plik aktualizacji jest za duży — pominięto.".into());
        }
    }
    Ok(out)
}

/// Nowy plik musi się uruchomić i podać właściwą wersję.
async fn selftest(exe: &Path, version: &str) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("--selftest").kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let output = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .map_err(|_| "Nowa wersja nie odpowiada — pominięto.".to_owned())?
        .map_err(|e| format!("Nowa wersja się nie uruchamia: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && text.trim() == format!("limbochat-engine {version}") {
        Ok(())
    } else {
        Err("Nowa wersja nie przeszła testu uruchomienia — pominięto.".into())
    }
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    let _ = path;
}

// ── Wersje ─────────────────────────────────────────────────────────────

fn parse_version(v: &str) -> Option<Vec<u64>> {
    let core = v.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let parts: Vec<u64> = core.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    (!parts.is_empty() && parts.len() <= 4).then_some(parts)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(mut a), Some(mut b)) => {
            let n = a.len().max(b.len());
            a.resize(n, 0);
            b.resize(n, 0);
            a > b
        }
        _ => false,
    }
}

// ── Podpisy ────────────────────────────────────────────────────────────

fn payload(version: &str, asset: &str, sha256_hex: &str) -> String {
    format!("{SIG_HEADER}\n{version}\n{asset}\n{sha256_hex}\n")
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn verify(sig_file: &str, key: &VerifyingKey, version: &str, asset: &str, bytes: &[u8]) -> Result<(), String> {
    const BAD: &str = "Podpis aktualizacji się nie zgadza — pominięto (ochrona przed podmienionym plikiem).";
    let lines: Vec<&str> = sig_file.lines().map(str::trim).collect();
    if lines.len() < 5 || lines[0] != SIG_HEADER {
        return Err(BAD.into());
    }
    if lines[1] != version || lines[2] != asset || lines[3] != sha256_hex(bytes) {
        return Err(BAD.into());
    }
    let sig: [u8; 64] = B64.decode(lines[4]).ok().and_then(|b| b.try_into().ok()).ok_or(BAD)?;
    key.verify(payload(version, asset, lines[3]).as_bytes(), &Signature::from_bytes(&sig)).map_err(|_| BAD.to_owned())
}

pub fn sign(key: &SigningKey, version: &str, asset: &str, bytes: &[u8]) -> String {
    let sha = sha256_hex(bytes);
    let p = payload(version, asset, &sha);
    let sig = key.sign(p.as_bytes());
    format!("{p}{}\n", B64.encode(sig.to_bytes()))
}

fn load_signing_key(path: &Path) -> Result<SigningKey, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("nie mogę odczytać klucza {}: {e}", path.display()))?;
    let bytes: [u8; 32] = B64.decode(text.trim()).ok().and_then(|b| b.try_into().ok()).ok_or("zły format klucza prywatnego")?;
    Ok(SigningKey::from_bytes(&bytes))
}

// ── Narzędzia wydawcy (wywoływane przez skrypty .bat) ──────────────────

/// `--keygen <plik>`: nowy klucz prywatny do pliku; na wyjściu klucz publiczny.
pub fn cli_keygen(path: &Path) -> i32 {
    if path.exists() {
        eprintln!("Plik {} już istnieje — nie nadpisuję klucza.", path.display());
        return 1;
    }
    let key = SigningKey::generate(&mut rand_core::OsRng);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, B64.encode(key.to_bytes())) {
        eprintln!("Nie mogę zapisać klucza: {e}");
        return 1;
    }
    println!("{}", B64.encode(key.verifying_key().to_bytes()));
    0
}

/// `--verify <plik> <wersja> <nazwa> <podpis>`: czy podpis pasuje do klucza publicznego
/// (`LIMBOCHAT_UPDATE_PUBKEY`). Wydanie z GitHuba sprawdza tak, czy klucz z sekretu
/// jest tym samym kluczem, którym podpisano poprzednie wersje. 0 = pasuje.
pub fn cli_verify(file: &Path, version: &str, asset: &str, sig: &Path) -> i32 {
    let Some(key) = public_key() else {
        eprintln!("Brak klucza publicznego (LIMBOCHAT_UPDATE_PUBKEY).");
        return 2;
    };
    let (Ok(bytes), Ok(sig)) = (std::fs::read(file), std::fs::read_to_string(sig)) else {
        eprintln!("Nie mogę odczytać pliku albo podpisu.");
        return 2;
    };
    match verify(&sig, &key, version, asset, &bytes) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `--pubkey <plik>`: klucz publiczny dla istniejącego klucza prywatnego.
pub fn cli_pubkey(path: &Path) -> i32 {
    match load_signing_key(path) {
        Ok(key) => {
            println!("{}", B64.encode(key.verifying_key().to_bytes()));
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `--sign <plik> <wersja> <nazwa-w-wydaniu> <klucz> <wyjście.sig>`
pub fn cli_sign(file: &Path, version: &str, asset: &str, key: &Path, out: &Path) -> i32 {
    let run = || -> Result<(), String> {
        let key = load_signing_key(key)?;
        let bytes = std::fs::read(file).map_err(|e| format!("nie mogę odczytać {}: {e}", file.display()))?;
        let sig = sign(&key, version, asset, &bytes);
        verify(&sig, &key.verifying_key(), version, asset, &bytes)?;
        std::fs::write(out, sig).map_err(|e| format!("nie mogę zapisać podpisu: {e}"))
    };
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

// ── Pliki w folderze silnika ───────────────────────────────────────────

/// Wersja, która nie wystartowała — nie próbujemy jej ponownie.
pub fn skip_version(dir: &Path, version: &str) {
    let _ = std::fs::write(dir.join("skip-version.txt"), version);
}

fn skipped(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("skip-version.txt")).ok().map(|s| s.trim().to_owned())
}

/// `current.txt` mówi wtyczce OBS, który plik silnika uruchamiać.
pub fn set_current(dir: &Path, exe: &Path) -> std::io::Result<()> {
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let tmp = dir.join("current.txt.tmp");
    std::fs::write(&tmp, name)?;
    std::fs::rename(tmp, dir.join("current.txt"))
}

pub fn current(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("current.txt")).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Usuwa stare wersje silnika (te, które nie są ani bieżące, ani uruchomione).
pub fn cleanup(dir: &Path) {
    let keep_current = current(dir);
    let running = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_engine = name.starts_with("limbochat-engine-") && (name.ends_with(EXE_SUFFIX) || name.ends_with(".download"));
        if is_engine && Some(&name) != keep_current.as_ref() && Some(&name) != running.as_ref() {
            // Plik może być jeszcze zajęty (Windows) — wtedy spróbujemy następnym razem.
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("0.6.0", "0.5.0"));
        assert!(is_newer("v1.0", "0.9.9"));
        assert!(is_newer("0.5.10", "0.5.9"));
        assert!(!is_newer("0.5.0", "0.5.0"));
        assert!(!is_newer("0.4.9", "0.5.0"), "bez cofania wersji");
        assert!(!is_newer("abc", "0.5.0"));
        assert!(is_newer("0.6.0-beta", "0.5.0"));
    }

    #[test]
    fn signatures_bind_file_version_and_name() {
        let key = SigningKey::generate(&mut rand_core::OsRng);
        let public = key.verifying_key();
        let file = b"nowy silnik";
        let sig = sign(&key, "0.6.0", ASSET, file);
        assert!(verify(&sig, &public, "0.6.0", ASSET, file).is_ok());
        assert!(verify(&sig, &public, "0.6.0", ASSET, b"podmieniony plik").is_err(), "inny plik");
        assert!(verify(&sig, &public, "0.7.0", ASSET, file).is_err(), "podpis starej wersji pod nowym tagiem");
        assert!(verify(&sig, &public, "0.6.0", "inny.exe", file).is_err());
        let other = SigningKey::generate(&mut rand_core::OsRng).verifying_key();
        assert!(verify(&sig, &other, "0.6.0", ASSET, file).is_err(), "obcy klucz");
        let tampered = sig.replace("0.6.0", "0.6.1");
        assert!(verify(&tampered, &public, "0.6.1", ASSET, file).is_err(), "zmieniona treść podpisu");
        assert!(verify("śmieci", &public, "0.6.0", ASSET, file).is_err());
    }
}
