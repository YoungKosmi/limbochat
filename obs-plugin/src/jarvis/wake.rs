//! Aktywacja głosem: „Hej Limbo”.
//!
//! Dwa etapy, żeby nie wysyłać do chmury wszystkiego, co mówisz na streamie:
//! 1. **Lokalnie** (ten moduł): mikrofon jest stale nasłuchiwany, ale tylko
//!    na Twoim komputerze. Każda wypowiedź jest porównywana z 3 nagraniami
//!    „Hej Limbo”, którymi nauczyłeś Jarvisa swojego głosu (MFCC + DTW).
//! 2. **W chmurze** (tylko kandydaci z etapu 1): krótkie nagranie trafia do
//!    rozpoznawania mowy i dopiero słowo „Limbo” w transkrypcji uruchamia
//!    Jarvisa. Reszta zdania („…zmień kategorię na GTA 4”) to od razu komenda.
//!
//! Nasłuch jest wstrzymywany, gdy Jarvis słucha komendy, myśli lub mówi
//! (żeby nie aktywował się własnym głosem).

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::{
    audio,
    dsp::{self, Frame, Mfcc},
};

const FRAME_MS: u32 = 20;
const PREROLL_FRAMES: usize = 15; // 300 ms przed początkiem mowy
const START_FRAMES: usize = 3;
const END_FRAMES: usize = 25; // 500 ms ciszy kończy wypowiedź
const MIN_FRAMES: usize = 15; // < 300 ms to nie „Hej Limbo”
const MAX_FRAMES: usize = 400; // 8 s
pub const ENROLL_SAMPLES: usize = 3;

/// Nauczony model: cechy 3 nagrań i ich wzajemne podobieństwo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeModel {
    pub templates: Vec<Vec<Vec<f32>>>,
    /// Największa odległość między nagraniami treningowymi.
    pub spread: f32,
}

impl WakeModel {
    fn frames(&self) -> Vec<Vec<Frame>> {
        self.templates
            .iter()
            .map(|t| {
                t.iter()
                    .map(|f| {
                        let mut a = [0.0f32; dsp::DIMS];
                        for (d, v) in f.iter().take(dsp::DIMS).enumerate() {
                            a[d] = *v;
                        }
                        a
                    })
                    .collect()
            })
            .collect()
    }
}

/// Czułość 1–3 → mnożnik progu (wyższa = łatwiej aktywować, więcej zapytań).
pub fn sensitivity_factor(level: u8) -> f32 {
    match level {
        1 => 1.15,
        3 => 1.6,
        _ => 1.35,
    }
}

fn model_path() -> Option<PathBuf> {
    crate::config::data_dir().map(|d| d.join("wake-hej-limbo.json"))
}

pub fn load_model() -> Option<WakeModel> {
    let text = std::fs::read_to_string(model_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_model(model: &WakeModel) -> anyhow::Result<()> {
    let path = model_path().ok_or_else(|| anyhow::anyhow!("brak folderu ustawień"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string(model)?)?;
    Ok(())
}

pub fn delete_model() {
    if let Some(path) = model_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Cechy jednego nagrania treningowego (WAV 16 kHz). Błąd = komunikat dla użytkownika.
pub fn enrollment_features(wav: &[u8]) -> Result<Vec<Frame>, String> {
    let (samples, rate) = audio::decode_wav(wav).ok_or("Nie udało się odczytać nagrania.")?;
    let samples = audio::resample(&samples, rate, dsp::RATE);
    let frames = dsp::trim(&Mfcc::new().features(&samples), true);
    let seconds = frames.len() as f32 / 100.0;
    if seconds < 0.25 {
        return Err("Za krótko — powiedz wyraźnie „Hej Limbo”.".into());
    }
    if seconds > 2.5 {
        return Err("Za długo — powiedz tylko „Hej Limbo”.".into());
    }
    Ok(frames)
}

/// Buduje model z nagrań treningowych.
pub fn build_model(samples: &[Vec<Frame>]) -> Result<WakeModel, String> {
    if samples.len() < 2 {
        return Err("Potrzebne są co najmniej 2 nagrania.".into());
    }
    let lengths: Vec<usize> = samples.iter().map(Vec::len).collect();
    let (min, max) = (*lengths.iter().min().unwrap_or(&0), *lengths.iter().max().unwrap_or(&0));
    if max > min * 2 {
        return Err("Nagrania bardzo się różnią długością — nagraj jeszcze raz, mówiąc tak samo.".into());
    }
    let mut spread: f32 = 0.0;
    for i in 0..samples.len() {
        for j in i + 1..samples.len() {
            spread = spread.max(dsp::dtw_full(&dsp::cmn(&samples[i]), &dsp::cmn(&samples[j])));
        }
    }
    Ok(WakeModel {
        templates: samples.iter().map(|t| t.iter().map(|f| f.to_vec()).collect()).collect(),
        // Dolna granica: trzy bardzo podobne nagrania nie mogą dać nierealnie ostrego progu.
        spread: spread.max(2.5),
    })
}

/// Odległość wypowiedzi od wzorców (mniejsza = bardziej podobna).
pub fn score(templates: &[Vec<Frame>], utterance_16k: &[f32], mfcc: &Mfcc) -> f32 {
    let frames = dsp::trim(&mfcc.features(utterance_16k), true);
    templates.iter().map(|t| dsp::dtw_prefix(t, &frames)).fold(f32::INFINITY, f32::min)
}

/// Wynik rozpoznania frazy w transkrypcji.
#[derive(Debug, PartialEq, Eq)]
pub struct WakeHit {
    /// Reszta zdania po „Hej Limbo” (jeśli coś powiedziano od razu).
    pub command: Option<String>,
}

/// Szuka „Limbo” wśród pierwszych słów transkrypcji.
pub fn parse_wake(text: &str) -> Option<WakeHit> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let clean = |t: &str| -> String {
        t.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .map(|c| match c {
                'ó' => 'o', 'ą' => 'a', 'ę' => 'e', 'ł' => 'l', other => other,
            })
            .collect()
    };
    // „Limbo” musi być pierwszym słowem albo drugim po powitaniu —
    // „gramy dziś w Limbo” to nie wezwanie.
    const GREETINGS: &[&str] = &["hej", "hey", "hejka", "ej", "ej", "hi", "halo", "siema", "okej", "ok", "okay", "yo", "jo", "elo", "no"];
    for i in 0..tokens.len().min(2) {
        if i == 1 && !GREETINGS.contains(&clean(tokens[0]).as_str()) {
            break;
        }
        let word = clean(tokens[i]);
        let next = tokens.get(i + 1).map(|t| clean(t)).unwrap_or_default();
        let consumed = if word.starts_with("limb") || word == "lembo" || word == "limbo" {
            1
        } else if word == "lim" && next.starts_with("bo") {
            2
        } else {
            continue;
        };
        let rest = tokens[i + consumed..].join(" ");
        let rest = rest.trim_start_matches([',', '.', '!', '?', ':', ';', '-', '—', ' ']).trim().to_owned();
        let command = (rest.chars().filter(|c| c.is_alphabetic()).count() >= 2).then_some(rest);
        return Some(WakeHit { command });
    }
    None
}

/// Działający nasłuch. Zatrzymuje się przy `drop`.
pub struct Listener {
    stop: Arc<AtomicBool>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Uruchamia nasłuch w osobnym wątku.
/// `paused()` — czy chwilowo ignorować dźwięk (Jarvis słucha/mówi),
/// `on_candidate(wav, odległość)` — wypowiedź podobna do „Hej Limbo”,
/// `on_error(komunikat)` — mikrofon niedostępny (nasłuch kończy się).
pub fn spawn(
    device: Option<String>,
    model: &WakeModel,
    sensitivity: u8,
    paused: impl Fn() -> bool + Send + 'static,
    on_candidate: impl Fn(Vec<u8>, f32) + Send + 'static,
    on_error: impl Fn(String) + Send + 'static,
) -> Listener {
    let stop = Arc::new(AtomicBool::new(false));
    let templates = model.frames();
    let threshold = model.spread * sensitivity_factor(sensitivity);
    let stop_thread = stop.clone();
    std::thread::Builder::new()
        .name("limbochat-wake".into())
        .spawn(move || {
            if let Err(e) = run(device, templates, threshold, &stop_thread, &paused, &on_candidate) {
                on_error(format!("{e:#}"));
            }
        })
        .ok();
    Listener { stop }
}

fn run(
    device: Option<String>,
    templates: Vec<Vec<Frame>>,
    threshold: f32,
    stop: &AtomicBool,
    paused: &dyn Fn() -> bool,
    on_candidate: &dyn Fn(Vec<u8>, f32),
) -> anyhow::Result<()> {
    let mfcc = Mfcc::new();
    let handle = |utterance: Vec<f32>, rate: u32| {
        let samples = audio::resample(&utterance, rate, dsp::RATE);
        let distance = score(&templates, &samples, &mfcc);
        tracing::debug!(distance, threshold, "wypowiedź sprawdzona");
        if distance < threshold {
            on_candidate(audio::encode_wav(&samples, dsp::RATE), distance);
        }
    };

    // Tryb testowy: plik WAV zamiast mikrofonu (odtwarzany raz, w czasie rzeczywistym).
    if let Ok(path) = std::env::var("LIMBOCHAT_WAKE_WAV") {
        let bytes = std::fs::read(&path)?;
        let (samples, rate) = audio::decode_wav(&bytes).ok_or_else(|| anyhow::anyhow!("zły plik WAV"))?;
        let mut seg = Segmenter::new(rate);
        let chunk = (rate * FRAME_MS / 1000) as usize;
        std::thread::sleep(Duration::from_millis(1500));
        for part in samples.chunks(chunk).chain(std::iter::repeat_n(&vec![0.0f32; chunk][..], 60)) {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if paused() {
                seg.reset();
            } else if let Some(u) = seg.push(part) {
                handle(u, rate);
            }
            std::thread::sleep(Duration::from_millis(FRAME_MS as u64));
        }
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
        return Ok(());
    }

    let input = audio::open_input(device.as_deref())?;
    let mut seg = Segmenter::new(input.rate);
    tracing::info!("nasłuch „Hej Limbo” aktywny");
    while !stop.load(Ordering::Relaxed) {
        match input.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                if paused() {
                    seg.reset();
                } else if let Some(u) = seg.push(&chunk) {
                    handle(u, input.rate);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("Mikrofon przestał przesyłać dźwięk"),
        }
    }
    drop(input);
    Ok(())
}

/// Dzieli ciągły dźwięk na wypowiedzi (z adaptacyjnym progiem szumu).
struct Segmenter {
    frame_len: usize,
    pending: Vec<f32>,
    preroll: VecDeque<Vec<f32>>,
    current: Vec<f32>,
    floor: f32,
    loud: usize,
    quiet: usize,
    frames: usize,
    speaking: bool,
}

impl Segmenter {
    fn new(rate: u32) -> Self {
        Self {
            frame_len: (rate * FRAME_MS / 1000) as usize,
            pending: Vec::new(),
            preroll: VecDeque::new(),
            current: Vec::new(),
            floor: 0.01,
            loud: 0,
            quiet: 0,
            frames: 0,
            speaking: false,
        }
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.preroll.clear();
        self.current.clear();
        self.loud = 0;
        self.quiet = 0;
        self.frames = 0;
        self.speaking = false;
    }

    /// Zwraca zakończoną wypowiedź (próbki w oryginalnej częstotliwości).
    fn push(&mut self, chunk: &[f32]) -> Option<Vec<f32>> {
        self.pending.extend_from_slice(chunk);
        let mut finished = None;
        while self.pending.len() >= self.frame_len {
            let frame: Vec<f32> = self.pending.drain(..self.frame_len).collect();
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
            let threshold = (self.floor * 3.0).max(0.015);

            if !self.speaking {
                // Szum tła powoli dostosowuje próg (wentylator, gra w tle…).
                self.floor = (self.floor * 0.95 + rms * 0.05).clamp(0.002, 0.2);
                self.preroll.push_back(frame);
                if self.preroll.len() > PREROLL_FRAMES {
                    self.preroll.pop_front();
                }
                self.loud = if rms > threshold { self.loud + 1 } else { 0 };
                if self.loud >= START_FRAMES {
                    self.speaking = true;
                    self.quiet = 0;
                    self.current = self.preroll.drain(..).flatten().collect();
                    self.frames = self.current.len() / self.frame_len;
                }
                continue;
            }

            self.current.extend_from_slice(&frame);
            self.frames += 1;
            self.quiet = if rms < threshold * 0.8 { self.quiet + 1 } else { 0 };
            if self.quiet >= END_FRAMES || self.frames >= MAX_FRAMES {
                self.speaking = false;
                self.loud = 0;
                let utterance = std::mem::take(&mut self.current);
                if self.frames >= MIN_FRAMES + END_FRAMES.min(self.quiet) {
                    finished = Some(utterance);
                }
                self.frames = 0;
            }
        }
        finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jarvis::dsp::tests::{OTHER, WAKE, synth};

    #[test]
    fn parses_wake_phrase_and_command() {
        assert_eq!(parse_wake("Hej Limbo, zmień kategorię na GTA 4.").unwrap().command.as_deref(), Some("zmień kategorię na GTA 4."));
        assert_eq!(parse_wake("Hej, Limbo!").unwrap().command, None);
        assert_eq!(parse_wake("hey limbó").unwrap().command, None);
        assert_eq!(parse_wake("Hej lim bo daj timeout spamerowi").unwrap().command.as_deref(), Some("daj timeout spamerowi"));
        assert!(parse_wake("hej ludzie, co tam").is_none());
        assert!(parse_wake("gramy dziś w Limbo, tę grę z chłopcem").is_none(), "„Limbo” daleko w zdaniu to nie wezwanie");
    }

    #[test]
    fn segmenter_splits_utterances() {
        let mut seg = Segmenter::new(16_000);
        let mut audio = vec![0.001f32; 16_000];
        audio.extend(synth(WAKE, 1.0, 0.0));
        audio.extend(vec![0.001f32; 16_000]);
        let mut found = Vec::new();
        for chunk in audio.chunks(160) {
            if let Some(u) = seg.push(chunk) {
                found.push(u.len());
            }
        }
        assert_eq!(found.len(), 1, "jedna wypowiedź");
        assert!(found[0] > 16_000 / 2, "zawiera całą frazę");
    }

    #[test]
    fn trained_model_accepts_phrase_and_rejects_other_speech() {
        let mfcc = Mfcc::new();
        let train: Vec<Vec<Frame>> = [1.0f32, 0.92, 1.08]
            .iter()
            .map(|&tempo| enrollment_features(&audio::encode_wav(&synth(WAKE, tempo, 0.02), 16_000)).unwrap())
            .collect();
        let model = build_model(&train).unwrap();
        let threshold = model.spread * sensitivity_factor(2);
        let templates = model.frames();

        let mut wake_then_command = synth(WAKE, 0.97, 0.02);
        wake_then_command.extend(synth(OTHER, 1.0, 0.02));
        eprintln!(
            "spread {:.3} próg {:.3} | fraza {:.3} | fraza+komenda {:.3} | inna {:.3}",
            model.spread,
            threshold,
            score(&templates, &synth(WAKE, 1.04, 0.02), &mfcc),
            score(&templates, &wake_then_command, &mfcc),
            score(&templates, &synth(OTHER, 1.0, 0.02), &mfcc)
        );
        assert!(score(&templates, &synth(WAKE, 1.04, 0.02), &mfcc) < threshold, "sama fraza");
        assert!(score(&templates, &wake_then_command, &mfcc) < threshold, "fraza + komenda");
        assert!(score(&templates, &synth(OTHER, 1.0, 0.02), &mfcc) > threshold, "inna wypowiedź");
    }

    #[test]
    fn enrollment_rejects_too_short_or_long() {
        assert!(enrollment_features(&audio::encode_wav(&vec![0.0; 1600], 16_000)).is_err());
        let long: Vec<f32> = (0..16_000 * 4).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        assert!(enrollment_features(&audio::encode_wav(&long, 16_000)).is_err());
    }
}
