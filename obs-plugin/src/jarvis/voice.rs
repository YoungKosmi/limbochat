//! Głos Jarvisa i sygnały dźwiękowe.
//!
//! Na Windows uruchamiamy JEDEN ukryty proces PowerShella z syntezatorem
//! mowy (System.Speech, polski głos systemowy, np. „Paulina”), który
//! czeka na polecenia na standardowym wejściu. Dzięki temu każda
//! odpowiedź zaczyna być mówiona od razu, bez kilkusekundowego startu.
//!
//! Protokół (jedna linia = jedno polecenie, treść w base64, bo polskie
//! znaki przez potok konsoli to proszenie się o kłopoty):
//! * `T <base64 UTF-8>` — powiedz tekst (przerywa poprzednią wypowiedź),
//! * `S <base64 WAV>`   — zagraj dźwięk,
//! * `X`                — przestań mówić.

#[cfg(windows)]
use std::io::Write;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use base64::Engine;

use super::audio::encode_wav;

#[derive(Debug, Clone, Copy)]
pub enum Chime {
    Listen,
    Stop,
    Error,
}

pub struct Voice {
    enabled: AtomicBool,
    /// `Some(true/false)` gdy wiadomo, czy jest polski głos systemowy.
    polish: Arc<Mutex<Option<bool>>>,
    #[cfg(windows)]
    process: Mutex<Option<std::process::Child>>,
}

impl Voice {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            polish: Arc::new(Mutex::new(None)),
            #[cfg(windows)]
            process: Mutex::new(None),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        if !enabled {
            self.stop();
        }
    }

    pub fn polish_voice(&self) -> Option<bool> {
        *self.polish.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn say(&self, text: &str) {
        if !self.enabled.load(Ordering::Relaxed) || text.trim().is_empty() {
            return;
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        self.send(&format!("T {b64}"));
    }

    pub fn chime(&self, chime: Chime) {
        // Dźwięki grają zawsze (nawet z wyłączonym głosem) — to jedyna
        // informacja zwrotna, gdy grasz na pełnym ekranie.
        let b64 = base64::engine::general_purpose::STANDARD.encode(chime_wav(chime));
        self.send(&format!("S {b64}"));
    }

    pub fn stop(&self) {
        self.send("X");
    }

    /// Szacowany czas wypowiedzi (do animacji „mówię”).
    pub fn estimate_seconds(text: &str) -> f32 {
        (text.chars().count() as f32 / 14.0).clamp(0.8, 20.0)
    }

    #[cfg(windows)]
    fn send(&self, line: &str) {
        let mut slot = self.process.lock().unwrap_or_else(|e| e.into_inner());
        for _ in 0..2 {
            if slot.is_none() {
                *slot = spawn_speaker(self.polish.clone());
            }
            let Some(child) = slot.as_mut() else { return };
            let written = child.stdin.as_mut().is_some_and(|stdin| writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_ok());
            if written {
                return;
            }
            // Proces padł — uruchom ponownie i spróbuj jeszcze raz.
            let _ = child.kill();
            *slot = None;
        }
    }

    #[cfg(not(windows))]
    fn send(&self, line: &str) {
        let preview: String = line.chars().take(40).collect();
        tracing::debug!("głos (tylko Windows): {preview}");
    }
}

#[cfg(windows)]
impl Drop for Voice {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
const SPEAKER_SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
$v = $s.GetInstalledVoices() | Where-Object { $_.Enabled -and $_.VoiceInfo.Culture.Name -eq 'pl-PL' } | Select-Object -First 1
if ($v) { $s.SelectVoice($v.VoiceInfo.Name) }
$s.Rate = 1
$s.Volume = 100
[Console]::Out.WriteLine("READY " + [bool]$v)
[Console]::Out.Flush()
$player = $null
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($line -eq $null) { break }
  if ($line.Length -lt 1) { continue }
  $cmd = $line.Substring(0, 1)
  $arg = if ($line.Length -gt 2) { $line.Substring(2) } else { '' }
  switch ($cmd) {
    'T' { $s.SpeakAsyncCancelAll(); $t = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($arg)); [void]$s.SpeakAsync($t) }
    'S' { $ms = New-Object System.IO.MemoryStream(,[Convert]::FromBase64String($arg)); $player = New-Object System.Media.SoundPlayer($ms); $player.Play() }
    'X' { $s.SpeakAsyncCancelAll() }
  }
}
"#;

#[cfg(windows)]
fn spawn_speaker(polish: Arc<Mutex<Option<bool>>>) -> Option<std::process::Child> {
    use std::{
        io::{BufRead, BufReader},
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let encoded = {
        let utf16: Vec<u8> = SPEAKER_SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
        base64::engine::general_purpose::STANDARD.encode(utf16)
    };
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| tracing::warn!("nie udało się uruchomić syntezatora mowy: {e}"))
        .ok()?;

    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(flag) = line.strip_prefix("READY ") {
                    let has_polish = flag.trim().eq_ignore_ascii_case("true");
                    if !has_polish {
                        tracing::warn!("brak polskiego głosu systemowego — Jarvis użyje domyślnego");
                    }
                    *polish.lock().unwrap_or_else(|e| e.into_inner()) = Some(has_polish);
                }
            }
        });
    }
    Some(child)
}

/// Krótkie, miękkie dźwięki generowane w kodzie (bez plików).
fn chime_wav(chime: Chime) -> Vec<u8> {
    const RATE: u32 = 22_050;
    let notes: &[(f32, f32)] = match chime {
        Chime::Listen => &[(660.0, 0.07), (990.0, 0.11)],
        Chime::Stop => &[(990.0, 0.06), (740.0, 0.10)],
        Chime::Error => &[(330.0, 0.10), (247.0, 0.16)],
    };
    let mut samples = Vec::new();
    for &(freq, length) in notes {
        let n = (RATE as f32 * length) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            // Szybki atak i wykładnicze wybrzmienie = „szklany” dźwięk.
            let envelope = (t / 0.006).min(1.0) * (-t * 18.0).exp();
            let tone = (t * freq * std::f32::consts::TAU).sin() + 0.25 * (t * freq * 2.0 * std::f32::consts::TAU).sin();
            samples.push(tone * envelope * 0.32);
        }
    }
    encode_wav(&samples, RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chimes_are_short_valid_wavs() {
        for chime in [Chime::Listen, Chime::Stop, Chime::Error] {
            let wav = chime_wav(chime);
            assert_eq!(&wav[0..4], b"RIFF");
            assert!(wav.len() < 20_000, "dźwięk powinien być krótki");
        }
    }

    #[test]
    fn speech_duration_estimate_is_sane() {
        assert!(Voice::estimate_seconds("Gotowe.") >= 0.8);
        assert!(Voice::estimate_seconds(&"a".repeat(140)) <= 20.0);
    }
}
