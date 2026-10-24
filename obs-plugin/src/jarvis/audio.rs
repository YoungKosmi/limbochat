//! Nagrywanie komendy głosowej z mikrofonu.
//!
//! * Przechwytywanie przez `cpal` (WASAPI w trybie współdzielonym, więc OBS
//!   dalej normalnie używa tego samego mikrofonu).
//! * Wykrywanie mowy (VAD) na podstawie głośności z automatycznym progiem:
//!   pierwsze ~250 ms to pomiar szumu tła, potem czekamy na mowę i kończymy
//!   po ~0,9 s ciszy. Można też zakończyć ręcznie (ponowny skrót).
//! * Wynik: WAV 16 kHz mono 16-bit — format, który najlepiej znoszą
//!   modele rozpoznawania mowy, a plik ma tylko ~32 KB na sekundę.
//!
//! Strumień `cpal` nie jest `Send`, więc całe nagrywanie działa w jednym,
//! zwykłym wątku (wywoływane przez `spawn_blocking`).

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use cpal::{
    SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

pub const TARGET_RATE: u32 = 16_000;
const FRAME_MS: u32 = 20;
const CALIBRATION_FRAMES: usize = 12; // ~250 ms
const START_FRAMES: usize = 3; // 60 ms głośności = początek mowy
const END_SILENCE_FRAMES: usize = 45; // 900 ms ciszy = koniec
const MAX_SECONDS: f32 = 12.0;
const MAX_HOLD_SECONDS: f32 = 30.0;
const NO_SPEECH_SECONDS: f32 = 6.0;
const MIN_THRESHOLD: f32 = 0.012;

pub struct Recording {
    pub wav: Vec<u8>,
    pub seconds: f32,
}

pub enum Outcome {
    Speech(Recording),
    NoSpeech,
    Cancelled,
}

pub struct Controls {
    /// Zakończ nagrywanie i wyślij to, co już jest (ponowny skrót).
    pub stop: Arc<AtomicBool>,
    /// Porzuć nagranie.
    pub cancel: Arc<AtomicBool>,
    /// Tryb „przytrzymaj, żeby mówić”: cisza nie kończy nagrania — kończy je puszczenie klawisza.
    pub hold: bool,
}

pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(e) => {
            tracing::warn!("nie udało się wypisać mikrofonów: {e}");
            Vec::new()
        }
    }
}

/// Nagrywa jedną komendę. Blokuje wątek do końca nagrania.
/// `level` dostaje głośność 0–1 kilkanaście razy na sekundę (do animacji).
pub fn record(device_name: Option<&str>, controls: &Controls, level: &dyn Fn(f32)) -> anyhow::Result<Outcome> {
    if let Ok(path) = std::env::var("LIMBOCHAT_JARVIS_WAV") {
        return fake_recording(&path, controls, level);
    }

    let Input { stream, rx, rate } = open_input(device_name)?;

    let frame_len = (rate * FRAME_MS / 1000) as usize;
    let mut vad = Vad::new();
    let mut samples: Vec<f32> = Vec::with_capacity(rate as usize * 8);
    let mut processed = 0usize;
    let mut last_level = Instant::now();
    let mut smoothed = 0.0f32;

    let state = loop {
        if controls.cancel.load(Ordering::Relaxed) {
            break VadState::Cancelled;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => samples.extend(chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(anyhow!("Mikrofon przestał przesyłać dźwięk")),
        }

        let mut state = VadState::Listening;
        while samples.len() - processed >= frame_len {
            let frame = &samples[processed..processed + frame_len];
            processed += frame_len;
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame_len as f32).sqrt();
            smoothed = smoothed * 0.6 + rms * 0.4;
            state = vad.push(rms);
            if !controls.hold && state != VadState::Listening && state != VadState::Speaking {
                break;
            }
        }
        if last_level.elapsed() >= Duration::from_millis(66) {
            level((smoothed * 9.0).sqrt().min(1.0));
            last_level = Instant::now();
        }

        let seconds = processed as f32 / rate as f32;
        if controls.stop.load(Ordering::Relaxed) {
            break if vad.started().is_some() { VadState::Done } else { VadState::NoSpeech };
        }
        if controls.hold {
            if seconds >= MAX_HOLD_SECONDS {
                break if vad.started().is_some() { VadState::Done } else { VadState::NoSpeech };
            }
            continue;
        }
        match state {
            VadState::Done | VadState::NoSpeech | VadState::Cancelled => break state,
            _ if seconds >= MAX_SECONDS => break if vad.started().is_some() { VadState::Done } else { VadState::NoSpeech },
            _ if seconds >= NO_SPEECH_SECONDS && vad.started().is_none() => break VadState::NoSpeech,
            _ => {}
        }
    };
    drop(stream);
    level(0.0);

    match state {
        VadState::Cancelled => Ok(Outcome::Cancelled),
        VadState::NoSpeech => Ok(Outcome::NoSpeech),
        _ => {
            // Zostawiamy 300 ms przed początkiem mowy i 200 ms po końcu.
            let start_frame = vad.started().unwrap_or(0).saturating_sub(15);
            // Przy przytrzymaniu bierzemy wszystko do puszczenia klawisza (pauzy w środku zdania).
            let end_frame = if controls.hold { usize::MAX } else { vad.ended().map_or(usize::MAX, |e| e + 10) };
            let from = (start_frame * frame_len).min(samples.len());
            let to = end_frame.saturating_mul(frame_len).min(samples.len());
            let speech = &samples[from..to.max(from)];
            let resampled = resample(speech, rate, TARGET_RATE);
            Ok(Outcome::Speech(Recording {
                seconds: resampled.len() as f32 / TARGET_RATE as f32,
                wav: encode_wav(&resampled, TARGET_RATE),
            }))
        }
    }
}

/// Otwarty mikrofon: strumień (trzymać przy życiu!) i próbki mono.
pub struct Input {
    pub stream: cpal::Stream,
    pub rx: mpsc::Receiver<Vec<f32>>,
    pub rate: u32,
}

/// Otwiera mikrofon (wybrany po nazwie albo domyślny) i uruchamia nagrywanie.
pub fn open_input(device_name: Option<&str>) -> anyhow::Result<Input> {
    let host = cpal::default_host();
    let device = match device_name.filter(|n| !n.is_empty()) {
        Some(name) => host
            .input_devices()?
            .find(|d| d.name().is_ok_and(|n| n == name))
            .or_else(|| host.default_input_device()),
        None => host.default_input_device(),
    }
    .ok_or_else(|| anyhow!("Nie znaleziono mikrofonu. Sprawdź, czy jest podłączony i włączony w Windows."))?;

    let supported = device.default_input_config().map_err(|e| mic_error(&e.to_string()))?;
    let rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let config = supported.config();

    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, channels, tx),
        SampleFormat::I16 => build::<i16>(&device, &config, channels, tx),
        SampleFormat::U16 => build::<u16>(&device, &config, channels, tx),
        SampleFormat::I32 => build::<i32>(&device, &config, channels, tx),
        SampleFormat::U8 => build::<u8>(&device, &config, channels, tx),
        other => return Err(anyhow!("Nieobsługiwany format mikrofonu: {other:?}")),
    }
    .map_err(|e| mic_error(&format!("{e:#}")))?;
    stream.play().map_err(|e| mic_error(&e.to_string()))?;
    Ok(Input { stream, rx, rate })
}

/// Odczyt prostego WAV PCM 16-bit (nasze nagrania i pliki testowe) → mono f32.
pub fn decode_wav(bytes: &[u8]) -> Option<(Vec<f32>, u32)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let (mut pos, mut channels, mut rate, mut bits) = (12usize, 1u16, 16_000u32, 16u16);
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().ok()?);
            rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().ok()?);
            bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().ok()?);
        } else if id == b"data" {
            if bits != 16 {
                return None;
            }
            let data = &bytes[body..(body + size).min(bytes.len())];
            let ch = channels.max(1) as usize;
            let samples = data
                .chunks_exact(2 * ch)
                .map(|frame| {
                    frame.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0).sum::<f32>() / ch as f32
                })
                .collect();
            return Some((samples, rate));
        }
        pos = body + size + (size & 1);
    }
    None
}

/// Najczęstsza przyczyna na Windows: wyłączony dostęp aplikacji do mikrofonu.
fn mic_error(detail: &str) -> anyhow::Error {
    anyhow!(
        "Nie mogę użyć mikrofonu. Sprawdź, czy jest podłączony i czy w Windows włączone jest \
         „Ustawienia → Prywatność → Mikrofon → Zezwalaj aplikacjom klasycznym na dostęp”. ({detail})"
    )
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, channels: usize, tx: mpsc::Sender<Vec<f32>>) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let stream = device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            // Uśredniamy kanały do mono.
            let mono: Vec<f32> = data
                .chunks(channels.max(1))
                .map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / frame.len() as f32)
                .collect();
            let _ = tx.send(mono);
        },
        |e| tracing::warn!("błąd mikrofonu: {e}"),
        None,
    )?;
    Ok(stream)
}

/// Tryb testowy: zamiast mikrofonu plik WAV (zmienna `LIMBOCHAT_JARVIS_WAV`).
fn fake_recording(path: &str, controls: &Controls, level: &dyn Fn(f32)) -> anyhow::Result<Outcome> {
    let wav = std::fs::read(path).with_context(|| format!("nie można wczytać {path}"))?;
    for i in 0..22 {
        if controls.cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Cancelled);
        }
        level(((i as f32 * 0.7).sin().abs() * 0.8 + 0.1).min(1.0));
        std::thread::sleep(Duration::from_millis(66));
    }
    level(0.0);
    Ok(Outcome::Speech(Recording { seconds: 1.5, wav }))
}

// ── Wykrywanie mowy ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VadState {
    Listening,
    Speaking,
    Done,
    NoSpeech,
    Cancelled,
}

struct Vad {
    frames: usize,
    noise_sum: f32,
    threshold: f32,
    loud_run: usize,
    quiet_run: usize,
    started: Option<usize>,
    ended: Option<usize>,
}

impl Vad {
    fn new() -> Self {
        Self { frames: 0, noise_sum: 0.0, threshold: MIN_THRESHOLD, loud_run: 0, quiet_run: 0, started: None, ended: None }
    }

    fn started(&self) -> Option<usize> {
        self.started
    }

    fn ended(&self) -> Option<usize> {
        self.ended
    }

    fn push(&mut self, rms: f32) -> VadState {
        let index = self.frames;
        self.frames += 1;

        if index < CALIBRATION_FRAMES {
            self.noise_sum += rms;
            if index + 1 == CALIBRATION_FRAMES {
                let noise = self.noise_sum / CALIBRATION_FRAMES as f32;
                self.threshold = (noise * 2.8).max(MIN_THRESHOLD);
            }
            // Ktoś zaczął mówić od razu — wyraźnie głośny dźwięk liczymy jako mowę.
            if rms > 0.06 {
                self.loud_run += 1;
            }
            if self.loud_run >= START_FRAMES && self.started.is_none() {
                self.started = Some(index + 1 - START_FRAMES);
            }
            return if self.started.is_some() { VadState::Speaking } else { VadState::Listening };
        }

        match self.started {
            None => {
                if rms > self.threshold {
                    self.loud_run += 1;
                    if self.loud_run >= START_FRAMES {
                        self.started = Some(index + 1 - START_FRAMES);
                        return VadState::Speaking;
                    }
                } else {
                    self.loud_run = 0;
                }
                VadState::Listening
            }
            Some(_) => {
                if rms < self.threshold * 0.8 {
                    self.quiet_run += 1;
                    if self.quiet_run >= END_SILENCE_FRAMES {
                        self.ended = Some(index + 1 - END_SILENCE_FRAMES);
                        return VadState::Done;
                    }
                } else {
                    self.quiet_run = 0;
                }
                VadState::Speaking
            }
        }
    }
}

// ── Przetwarzanie dźwięku ──────────────────────────────────────────────

/// Zmiana częstotliwości próbkowania (interpolacja liniowa z prostym
/// uśrednianiem przy zmniejszaniu — wystarczające dla mowy).
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 {
        return Vec::new();
    }
    if from == to {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = (input.len() as f64 / ratio).floor() as usize;
    let window = ratio.max(1.0);
    (0..out_len)
        .map(|i| {
            let center = i as f64 * ratio;
            if window > 1.0 {
                // Średnia z okna = filtr dolnoprzepustowy przeciw aliasingowi.
                let start = center.floor() as usize;
                let end = ((center + window).ceil() as usize).min(input.len());
                let slice = &input[start.min(end)..end];
                slice.iter().sum::<f32>() / slice.len().max(1) as f32
            } else {
                let idx = center.floor() as usize;
                let frac = (center - idx as f64) as f32;
                let a = input[idx.min(input.len() - 1)];
                let b = input[(idx + 1).min(input.len() - 1)];
                a + (b - a) * frac
            }
        })
        .collect()
}

pub fn encode_wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // rozmiar bloku fmt
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // bajty na sekundę
    out.extend_from_slice(&2u16.to_le_bytes()); // wyrównanie bloku
    out.extend_from_slice(&16u16.to_le_bytes()); // bity na próbkę
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_detects_speech_between_silences() {
        let mut vad = Vad::new();
        let mut states = Vec::new();
        for _ in 0..30 {
            states.push(vad.push(0.002)); // cisza: kalibracja + oczekiwanie
        }
        assert_eq!(*states.last().unwrap(), VadState::Listening);
        for _ in 0..40 {
            states.push(vad.push(0.08)); // mowa
        }
        assert_eq!(vad.started(), Some(30));
        let mut last = VadState::Speaking;
        for _ in 0..60 {
            last = vad.push(0.002);
            if last == VadState::Done {
                break;
            }
        }
        assert_eq!(last, VadState::Done);
        assert_eq!(vad.ended(), Some(70));
    }

    #[test]
    fn vad_ignores_steady_background_noise() {
        let mut vad = Vad::new();
        for _ in 0..200 {
            assert_ne!(vad.push(0.03), VadState::Speaking, "stały szum (np. wentylator) to nie mowa");
        }
    }

    #[test]
    fn resamples_48k_to_16k() {
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = resample(&input, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn wav_round_trip() {
        let wav = encode_wav(&[0.0, 0.5, -0.5, 0.25], 16_000);
        let (samples, rate) = decode_wav(&wav).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(samples.len(), 4);
        assert!((samples[1] - 0.5).abs() < 0.001 && (samples[2] + 0.5).abs() < 0.001);
    }

    #[test]
    fn wav_header_is_valid() {
        let wav = encode_wav(&[0.0, 0.5, -0.5], 16_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    }
}
