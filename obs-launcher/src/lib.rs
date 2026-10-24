//! Wtyczka OBS LiMBOCHAT (`limbochat.dll`).
//!
//! Cała praca dzieje się w osobnym procesie — silniku `limbochat-engine`.
//! Wtyczka tylko:
//! 1. przy starcie OBS instaluje wbudowany silnik (jeśli jest nowszy niż
//!    zainstalowany) w `%LOCALAPPDATA%\LiMBOCHAT\engine`,
//! 2. uruchamia silnik wskazany w `current.txt` z numerem procesu OBS,
//! 3. przy zamknięciu OBS prosi silnik o zakończenie.
//!
//! Dzięki temu silnik może się sam zaktualizować w trakcie streama, a OBS
//! nie musi być restartowany. Ta biblioteka nie zależy od żadnych crate'ów
//! i nie wywołuje funkcji libobs — zmienia się bardzo rzadko.

use std::{
    ffi::{c_char, c_void},
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    panic::catch_unwind,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const PORT: u16 = 47250;
static ENGINE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/engine.bin"));
const BUNDLED_VERSION: &str = env!("BUNDLED_ENGINE_VERSION");
const EXE_SUFFIX: &str = if cfg!(windows) { ".exe" } else { "" };

// ── Eksporty wymagane przez OBS ────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_set_pointer(_module: *mut c_void) {}

/// API OBS 28.0 — OBS odrzuca tylko wtyczki zbudowane pod NOWSZĄ wersję.
#[unsafe(no_mangle)]
pub extern "C" fn obs_module_ver() -> u32 {
    28 << 24
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_load() -> bool {
    // Panika nie może przejść przez granicę FFI; OBS ma działać mimo błędu.
    let _ = catch_unwind(start);
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_unload() {
    let _ = catch_unwind(stop);
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_name() -> *const c_char {
    c"LiMBOCHAT".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_description() -> *const c_char {
    c"Czat Twitcha z 7TV, moderacją i LimboAI jako panel OBS (http://127.0.0.1:47250)".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_author() -> *const c_char {
    c"LiMBOCHAT".as_ptr()
}

// ── Start i stop ───────────────────────────────────────────────────────

fn start() {
    let dir = engine_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log(&format!("nie mogę utworzyć {}: {e}", dir.display()));
        return;
    }
    install_bundled(&dir);
    let Some(exe) = current_engine(&dir) else {
        log("brak silnika — zainstaluj wtyczkę ponownie");
        return;
    };
    let mut cmd = Command::new(&exe);
    cmd.args(["--obs-pid", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0008); // CREATE_NO_WINDOW | DETACHED_PROCESS
    }
    match cmd.spawn() {
        Ok(child) => log(&format!("uruchomiono silnik {} (pid {})", exe.display(), child.id())),
        Err(e) => log(&format!("nie mogę uruchomić silnika {}: {e}", exe.display())),
    }
}

fn stop() {
    let token = std::fs::read_to_string(engine_dir().join("control.token")).unwrap_or_default();
    let request = format!(
        "POST /control/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\nX-LiMBOCHAT-Token: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        token.trim()
    );
    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    // Krótkie limity — nie blokujemy zamykania OBS. Silnik i tak zakończy
    // się sam, gdy zauważy, że OBS zniknął.
    if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
        let _ = stream.write_all(request.as_bytes());
        let _ = stream.read(&mut [0u8; 256]);
    }
}

// ── Pliki silnika ──────────────────────────────────────────────────────

fn engine_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LIMBOCHAT_ENGINE_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    base.unwrap_or_else(std::env::temp_dir).join("LiMBOCHAT").join("engine")
}

fn current_name(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("current.txt")).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

fn current_engine(dir: &Path) -> Option<PathBuf> {
    let name = current_name(dir)?;
    // Tylko nazwa pliku w folderze silnika (bez ścieżek z zewnątrz).
    if name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    Some(dir.join(name)).filter(|p| p.exists())
}

/// Wersja z nazwy pliku `limbochat-engine-0.6.0.exe`.
fn version_of(name: &str) -> Option<Vec<u64>> {
    let v = name.strip_prefix("limbochat-engine-")?.strip_suffix(EXE_SUFFIX).unwrap_or(name);
    v.split('.').map(|p| p.parse().ok()).collect()
}

fn parse(v: &str) -> Option<Vec<u64>> {
    v.split(['-', '+']).next()?.split('.').map(|p| p.parse().ok()).collect()
}

/// Instaluje silnik wbudowany w wtyczkę, jeśli nie ma żadnego albo jest starszy.
/// (Nowszy, pobrany przez autoaktualizację, zostaje.)
fn install_bundled(dir: &Path) {
    if ENGINE.is_empty() {
        return;
    }
    let installed = current_engine(dir).and_then(|p| version_of(p.file_name()?.to_str()?));
    let bundled = parse(BUNDLED_VERSION);
    if installed.is_some() && installed >= bundled {
        return;
    }
    let name = format!("limbochat-engine-{BUNDLED_VERSION}{EXE_SUFFIX}");
    let path = dir.join(&name);
    let tmp = dir.join(format!("{name}.download"));
    let written = std::fs::write(&tmp, ENGINE).and_then(|_| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, &path)
    });
    match written.and_then(|_| std::fs::write(dir.join("current.txt"), &name)) {
        Ok(()) => log(&format!("zainstalowano silnik {BUNDLED_VERSION}")),
        Err(e) => log(&format!("nie mogę zainstalować silnika: {e}")),
    }
}

fn log(line: &str) {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    let Some(dir) = base.map(|b| b.join("LiMBOCHAT")) else { return };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("obs-plugin.log")) {
        let _ = writeln!(f, "[wtyczka OBS] {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_from_file_names() {
        let n = |v: &str| format!("limbochat-engine-{v}{EXE_SUFFIX}");
        assert_eq!(version_of(&n("0.6.0")), Some(vec![0, 6, 0]));
        assert!(version_of(&n("0.10.0")) > version_of(&n("0.9.9")));
        assert_eq!(version_of("inny.exe"), None);
    }
}
