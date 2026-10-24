//! Operacje instalatora: instalacja, aktualizacja i dezinstalacja.
//!
//! Instalujemy „dla bieżącego użytkownika” — tak jak Discord czy VS Code:
//! * pliki: `%LOCALAPPDATA%\Programs\LiMBOCHAT`,
//! * skróty: menu Start (+ opcjonalnie pulpit),
//! * wpis w „Aplikacje i funkcje” w gałęzi rejestru HKCU.
//!
//! Nic z tego nie wymaga uprawnień administratora, więc Windows nie pokaże
//! okna UAC. Skróty i rejestr obsługujemy przez PowerShell (jest w każdym
//! Windows 10/11), a skrypt przekazujemy jako `-EncodedCommand`, żeby
//! ścieżki ze spacjami czy apostrofami nie psuły cudzysłowów.

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

use flate2::read::DeflateDecoder;

/// Skompresowany LiMBOCHAT.exe, przygotowany przez build.rs.
static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin"));
const PAYLOAD_SIZE: u64 = parse_size(env!("LIMBOCHAT_PAYLOAD_SIZE"));
static ICON: &[u8] = include_bytes!("../../assets/icon.ico");

pub const VERSION: &str = env!("LIMBOCHAT_VERSION");
pub const APP_NAME: &str = "LiMBOCHAT";
const UNINSTALL_KEY: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\LiMBOCHAT";
const KEYRING_SERVICE: &str = "limbochat";
const KEYRING_USER: &str = "twitch-oauth-token";

const fn parse_size(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut n = 0u64;
    while i < bytes.len() {
        n = n * 10 + (bytes[i] - b'0') as u64;
        i += 1;
    }
    n
}

pub fn app_size_mb() -> f32 {
    PAYLOAD_SIZE as f32 / 1_048_576.0
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
    pub exe: PathBuf,
    pub icon: PathBuf,
    pub uninstaller: PathBuf,
}

impl Paths {
    pub fn detect() -> Option<Self> {
        let base = directories::BaseDirs::new()?;
        let dir = base.data_local_dir().join("Programs").join(APP_NAME);
        let exe_name = if cfg!(windows) { "LiMBOCHAT.exe" } else { "LiMBOCHAT" };
        let uninstaller_name = if cfg!(windows) { "uninstall.exe" } else { "uninstall" };
        Some(Self {
            exe: dir.join(exe_name),
            icon: dir.join("icon.ico"),
            uninstaller: dir.join(uninstaller_name),
            dir,
        })
    }

    pub fn is_installed(&self) -> bool {
        self.exe.exists()
    }
}

/// Postęp wysyłany do UI z wątku roboczego.
pub enum Progress {
    /// Rozpoczęto krok o danym indeksie (z listy `*_STEPS`).
    Step(usize),
    /// Postęp w obrębie bieżącego kroku (0–1).
    Fraction(f32),
    /// Coś poszło nie do końca dobrze, ale instalacja trwa (np. brak skrótu).
    Warning(String),
    Done,
    Failed(String),
}

pub const INSTALL_STEPS: &[&str] = &[
    "Przygotowanie folderu",
    "Rozpakowywanie aplikacji",
    "Tworzenie skrótów",
    "Rejestracja w systemie",
];

pub const UNINSTALL_STEPS: &[&str] = &[
    "Usuwanie skrótów",
    "Usuwanie wpisu z systemu",
    "Usuwanie plików aplikacji",
    "Porządkowanie ustawień",
];

pub struct InstallOptions {
    pub desktop_shortcut: bool,
}

pub fn install(paths: &Paths, options: &InstallOptions, report: &dyn Fn(Progress)) {
    match run_install(paths, options, report) {
        Ok(()) => report(Progress::Done),
        Err(e) => report(Progress::Failed(e)),
    }
}

fn run_install(paths: &Paths, options: &InstallOptions, report: &dyn Fn(Progress)) -> Result<(), String> {
    report(Progress::Step(0));
    fs::create_dir_all(&paths.dir).map_err(|e| format!("Nie można utworzyć folderu {}: {e}", paths.dir.display()))?;

    report(Progress::Step(1));
    // Rozpakowujemy do pliku tymczasowego i dopiero na końcu podmieniamy —
    // przerwana instalacja nie zostawi uszkodzonego LiMBOCHAT.exe.
    let temp = paths.exe.with_extension("new");
    {
        let mut decoder = DeflateDecoder::new(PAYLOAD);
        let mut out = fs::File::create(&temp).map_err(|e| format!("Nie można zapisać pliku: {e}"))?;
        let mut buffer = vec![0u8; 256 * 1024];
        let mut written = 0u64;
        loop {
            let n = decoder.read(&mut buffer).map_err(|e| format!("Uszkodzony instalator: {e}"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buffer[..n]).map_err(|e| format!("Błąd zapisu (brak miejsca na dysku?): {e}"))?;
            written += n as u64;
            report(Progress::Fraction(written as f32 / PAYLOAD_SIZE.max(1) as f32));
        }
        out.sync_all().ok();
    }
    if let Err(e) = fs::rename(&temp, &paths.exe) {
        let _ = fs::remove_file(&temp);
        return Err(if paths.exe.exists() {
            "LiMBOCHAT jest uruchomiony. Zamknij aplikację i spróbuj ponownie.".to_owned()
        } else {
            format!("Nie można zainstalować aplikacji: {e}")
        });
    }
    make_executable(&paths.exe);
    fs::write(&paths.icon, ICON).map_err(|e| format!("Nie można zapisać ikony: {e}"))?;

    // Kopia instalatora służy jako dezinstalator (uruchamiany z `--uninstall`).
    if let Ok(me) = std::env::current_exe() {
        if !same_file(&me, &paths.uninstaller) {
            if let Err(e) = fs::copy(&me, &paths.uninstaller) {
                report(Progress::Warning(format!("Nie udało się zapisać dezinstalatora: {e}")));
            }
            make_executable(&paths.uninstaller);
        }
    }

    report(Progress::Step(2));
    if let Err(e) = create_shortcuts(paths, options.desktop_shortcut) {
        report(Progress::Warning(format!("Nie udało się utworzyć skrótów: {e}")));
    }

    report(Progress::Step(3));
    if let Err(e) = register_uninstaller(paths) {
        report(Progress::Warning(format!("Nie udało się dodać wpisu w „Aplikacje i funkcje”: {e}")));
    }
    Ok(())
}

pub struct UninstallOptions {
    pub remove_data: bool,
}

pub fn uninstall(paths: &Paths, options: &UninstallOptions, report: &dyn Fn(Progress)) {
    match run_uninstall(paths, options, report) {
        Ok(()) => report(Progress::Done),
        Err(e) => report(Progress::Failed(e)),
    }
}

fn run_uninstall(paths: &Paths, options: &UninstallOptions, report: &dyn Fn(Progress)) -> Result<(), String> {
    report(Progress::Step(0));
    if let Err(e) = remove_shortcuts() {
        report(Progress::Warning(format!("Nie udało się usunąć skrótów: {e}")));
    }

    report(Progress::Step(1));
    if let Err(e) = unregister_uninstaller() {
        report(Progress::Warning(format!("Nie udało się usunąć wpisu z systemu: {e}")));
    }

    report(Progress::Step(2));
    if paths.exe.exists() {
        fs::remove_file(&paths.exe)
            .map_err(|_| "LiMBOCHAT jest uruchomiony. Zamknij aplikację i spróbuj ponownie.".to_owned())?;
    }
    let _ = fs::remove_file(&paths.icon);

    report(Progress::Step(3));
    if options.remove_data {
        // Ustawienia eframe i flaga szkła leżą w %APPDATA%\LiMBOCHAT.
        if let Some(base) = directories::BaseDirs::new() {
            for name in ["LiMBOCHAT", "limbochat"] {
                let dir = base.config_dir().join(name);
                if dir.exists() {
                    let _ = fs::remove_dir_all(dir);
                }
            }
        }
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => report(Progress::Warning(format!("Nie udało się usunąć zapisanego logowania: {e}"))),
            }
        }
    }
    Ok(())
}

/// Dezinstalator nie może usunąć sam siebie, dopóki działa. Zlecamy więc
/// systemowi usunięcie folderu ~2 s po zamknięciu okna.
pub fn schedule_self_removal(dir: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = format!("/C ping 127.0.0.1 -n 3 > nul & rmdir /S /Q \"{}\"", dir.display());
        let _ = Command::new("cmd").raw_arg(script).creation_flags(CREATE_NO_WINDOW).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = fs::remove_dir_all(dir);
    }
}

pub fn launch(paths: &Paths) -> Result<(), String> {
    Command::new(&paths.exe)
        .current_dir(&paths.dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Nie udało się uruchomić aplikacji: {e}"))
}

// ── Skróty i rejestr (Windows) ─────────────────────────────────────────

/// Łańcuch w apostrofach dla PowerShella (apostrof podwajamy).
fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn create_shortcuts(paths: &Paths, desktop: bool) -> Result<(), String> {
    let exe = ps_quote(&paths.exe.display().to_string());
    let dir = ps_quote(&paths.dir.display().to_string());
    let icon = ps_quote(&paths.icon.display().to_string());
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$shell = New-Object -ComObject WScript.Shell
function New-LimboShortcut($folder) {{
    $link = $shell.CreateShortcut((Join-Path $folder 'LiMBOCHAT.lnk'))
    $link.TargetPath = {exe}
    $link.WorkingDirectory = {dir}
    $link.IconLocation = {icon}
    $link.Description = 'LiMBOCHAT — czat Twitcha z 7TV'
    $link.Save()
}}
New-LimboShortcut ([Environment]::GetFolderPath('Programs'))
if (${desktop}) {{ New-LimboShortcut ([Environment]::GetFolderPath('Desktop')) }}
"#,
        desktop = if desktop { "true" } else { "false" },
    );
    powershell(&script)
}

fn remove_shortcuts() -> Result<(), String> {
    powershell(
        r#"
foreach ($folder in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) {
    Remove-Item -LiteralPath (Join-Path $folder 'LiMBOCHAT.lnk') -Force -ErrorAction SilentlyContinue
}
"#,
    )
}

fn register_uninstaller(paths: &Paths) -> Result<(), String> {
    let uninstall_cmd = format!("\"{}\" --uninstall", paths.uninstaller.display());
    let size_kb = PAYLOAD_SIZE / 1024;
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$key = {key}
New-Item -Path $key -Force | Out-Null
Set-ItemProperty -Path $key -Name DisplayName -Value 'LiMBOCHAT'
Set-ItemProperty -Path $key -Name DisplayVersion -Value {version}
Set-ItemProperty -Path $key -Name Publisher -Value 'LiMBOCHAT'
Set-ItemProperty -Path $key -Name DisplayIcon -Value {icon}
Set-ItemProperty -Path $key -Name InstallLocation -Value {dir}
Set-ItemProperty -Path $key -Name UninstallString -Value {uninstall}
Set-ItemProperty -Path $key -Name QuietUninstallString -Value {uninstall}
New-ItemProperty -Path $key -Name NoModify -Value 1 -PropertyType DWord -Force | Out-Null
New-ItemProperty -Path $key -Name NoRepair -Value 1 -PropertyType DWord -Force | Out-Null
New-ItemProperty -Path $key -Name EstimatedSize -Value {size_kb} -PropertyType DWord -Force | Out-Null
"#,
        key = ps_quote(UNINSTALL_KEY),
        version = ps_quote(VERSION),
        icon = ps_quote(&paths.icon.display().to_string()),
        dir = ps_quote(&paths.dir.display().to_string()),
        uninstall = ps_quote(&uninstall_cmd),
    );
    powershell(&script)
}

fn unregister_uninstaller() -> Result<(), String> {
    powershell(&format!(
        "Remove-Item -Path {} -Recurse -Force -ErrorAction SilentlyContinue",
        ps_quote(UNINSTALL_KEY)
    ))
}

#[cfg(windows)]
fn powershell(script: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"])
        .arg(encode_powershell(script))
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("nie można uruchomić PowerShella: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("nieznany błąd").to_owned())
    }
}

/// Poza Windowsem skróty i rejestr nie istnieją — kroki są pomijane.
#[cfg(not(windows))]
fn powershell(_script: &str) -> Result<(), String> {
    Ok(())
}

/// `-EncodedCommand` przyjmuje skrypt w UTF-16LE zakodowany base64.
#[cfg_attr(not(windows), allow(dead_code))]
fn encode_powershell(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn powershell_encoding_is_utf16le() {
        // "a" w UTF-16LE to bajty 61 00 → base64 "YQA="
        assert_eq!(encode_powershell("a"), "YQA=");
    }

    #[test]
    fn quotes_paths_with_apostrophes() {
        assert_eq!(ps_quote(r"C:\Users\O'Brien"), r"'C:\Users\O''Brien'");
    }

    #[test]
    fn payload_decompresses_to_declared_size() {
        let mut decoder = DeflateDecoder::new(PAYLOAD);
        let mut total = 0u64;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = decoder.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            total += n as u64;
        }
        assert_eq!(total, PAYLOAD_SIZE);
    }
}
