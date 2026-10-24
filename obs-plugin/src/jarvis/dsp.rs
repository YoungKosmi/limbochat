//! Cechy dźwięku i porównywanie wypowiedzi dla detektora „Hej Limbo”.
//!
//! * MFCC — klasyczny „odcisk” barwy głosu: 25 ms ramki co 10 ms, filtry
//!   w skali melowej (jak ludzki słuch), logarytm i DCT → 12 współczynników.
//! * DTW (dynamic time warping) — porównuje dwie sekwencje cech, pozwalając
//!   na różne tempo mówienia („Heeej Limbo” vs „hej limbo”).
//!
//! Wszystko działa lokalnie i zajmuje ułamek milisekundy na wypowiedź.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex};

pub const RATE: u32 = 16_000;
pub const DIMS: usize = 12;
const FRAME: usize = 400; // 25 ms
const HOP: usize = 160; // 10 ms
const NFFT: usize = 512;
const MELS: usize = 26;

pub type Frame = [f32; DIMS];

pub struct Features {
    pub frames: Vec<Frame>,
    /// Logarytm energii ramki (do przycinania ciszy).
    pub energy: Vec<f32>,
}

pub struct Mfcc {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    mel: Vec<Vec<(usize, f32)>>,
    dct: Vec<[f32; MELS]>,
}

impl Default for Mfcc {
    fn default() -> Self {
        Self::new()
    }
}

impl Mfcc {
    pub fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(NFFT);
        let window = (0..FRAME)
            .map(|i| 0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / (FRAME - 1) as f32).cos())
            .collect();

        // Trójkątne filtry melowe 20–7600 Hz.
        let hz_to_mel = |hz: f32| 2595.0 * (1.0 + hz / 700.0).log10();
        let mel_to_hz = |mel: f32| 700.0 * (10f32.powf(mel / 2595.0) - 1.0);
        let (lo, hi) = (hz_to_mel(20.0), hz_to_mel(7600.0));
        let points: Vec<usize> = (0..MELS + 2)
            .map(|i| {
                let hz = mel_to_hz(lo + (hi - lo) * i as f32 / (MELS + 1) as f32);
                ((NFFT + 1) as f32 * hz / RATE as f32).floor() as usize
            })
            .collect();
        let mel = (1..=MELS)
            .map(|m| {
                let (left, center, right) = (points[m - 1], points[m], points[m + 1]);
                let mut weights = Vec::new();
                for k in left..right.max(left + 1) {
                    let w = if k < center {
                        (k - left) as f32 / (center - left).max(1) as f32
                    } else {
                        (right - k) as f32 / (right - center).max(1) as f32
                    };
                    if w > 0.0 && k <= NFFT / 2 {
                        weights.push((k, w));
                    }
                }
                weights
            })
            .collect();

        // DCT-II; pomijamy współczynnik 0 (zależy tylko od głośności).
        let dct = (1..=DIMS)
            .map(|k| {
                let mut row = [0.0f32; MELS];
                for (n, v) in row.iter_mut().enumerate() {
                    *v = (std::f32::consts::PI * k as f32 * (n as f32 + 0.5) / MELS as f32).cos();
                }
                row
            })
            .collect();

        Self { fft, window, mel, dct }
    }

    /// Cechy dla sygnału 16 kHz mono.
    pub fn features(&self, samples: &[f32]) -> Features {
        let mut frames = Vec::new();
        let mut energy = Vec::new();
        if samples.len() < FRAME {
            return Features { frames, energy };
        }
        let mut buf = vec![Complex::new(0.0f32, 0.0); NFFT];
        let mut start = 0;
        while start + FRAME <= samples.len() {
            for (i, slot) in buf.iter_mut().enumerate() {
                *slot = if i < FRAME {
                    // Preemfaza wzmacnia wysokie częstotliwości (spółgłoski).
                    let prev = if start + i > 0 { samples[start + i - 1] } else { 0.0 };
                    Complex::new((samples[start + i] - 0.97 * prev) * self.window[i], 0.0)
                } else {
                    Complex::new(0.0, 0.0)
                };
            }
            self.fft.process(&mut buf);
            let power: Vec<f32> = buf[..=NFFT / 2].iter().map(|c| c.norm_sqr() / NFFT as f32).collect();
            let total: f32 = power.iter().sum();
            energy.push(total.max(1e-10).ln());

            let mut logmel = [0.0f32; MELS];
            for (m, filter) in self.mel.iter().enumerate() {
                let e: f32 = filter.iter().map(|&(k, w)| power[k] * w).sum();
                logmel[m] = e.max(1e-10).ln();
            }
            let mut frame = [0.0f32; DIMS];
            for (d, row) in self.dct.iter().enumerate() {
                frame[d] = row.iter().zip(logmel.iter()).map(|(a, b)| a * b).sum();
            }
            frames.push(frame);
            start += HOP;
        }
        Features { frames, energy }
    }
}

/// Przycina ciszę na początku i (opcjonalnie) na końcu.
pub fn trim(features: &Features, trim_end: bool) -> Vec<Frame> {
    let n = features.energy.len();
    if n == 0 {
        return Vec::new();
    }
    let mut sorted = features.energy.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let floor = sorted[n / 10];
    let peak = sorted[n - 1];
    let threshold = (floor + 2.5).max(peak - 6.0);
    let first = features.energy.iter().position(|&e| e > threshold).unwrap_or(0);
    let last = if trim_end { features.energy.iter().rposition(|&e| e > threshold).unwrap_or(n - 1) } else { n - 1 };
    features.frames[first..=last.max(first)].to_vec()
}

/// Normalizacja średniej (usuwa wpływ mikrofonu i akustyki pokoju).
pub fn cmn(frames: &[Frame]) -> Vec<Frame> {
    cmn_by(frames, frames.len())
}

/// Jak `cmn`, ale średnia liczona z pierwszych `k` ramek.
pub fn cmn_by(frames: &[Frame], k: usize) -> Vec<Frame> {
    if frames.is_empty() {
        return Vec::new();
    }
    let k = k.clamp(1, frames.len());
    let mut mean = [0.0f32; DIMS];
    for f in &frames[..k] {
        for d in 0..DIMS {
            mean[d] += f[d];
        }
    }
    for m in &mut mean {
        *m /= k as f32;
    }
    frames
        .iter()
        .map(|f| {
            let mut out = [0.0f32; DIMS];
            for d in 0..DIMS {
                out[d] = f[d] - mean[d];
            }
            out
        })
        .collect()
}

fn dist(a: &Frame, b: &Frame) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
}

/// DTW dwóch całych wypowiedzi (do porównania nagrań przy nauce).
pub fn dtw_full(a: &[Frame], b: &[Frame]) -> f32 {
    dtw(a, b, 0, (b.len(), b.len()))
}

/// DTW wzorca z POCZĄTKIEM wypowiedzi: start może się przesunąć o kilka
/// ramek, a koniec dopasowania leży gdziekolwiek w rozsądnym zakresie —
/// dzięki temu „Hej Limbo, zmień kategorię…” też pasuje.
pub fn dtw_prefix(template: &[Frame], utterance: &[Frame]) -> f32 {
    let n = template.len();
    if n == 0 || utterance.len() < n / 2 {
        return f32::INFINITY;
    }
    let window = utterance.len().min(n * 8 / 5 + 25);
    // Średnią liczymy tylko z części, w której spodziewamy się frazy —
    // dalsza komenda nie może przesunąć normalizacji.
    let utter = cmn_by(&utterance[..window], n.min(window));
    let tmpl = cmn(template);
    let end_range = ((n * 3 / 5).max(1), window);
    dtw(&tmpl, &utter, 25, end_range)
}

/// Wspólna implementacja: `start_slack` = ile ramek na początku `b` można
/// pominąć, `end_range` = gdzie w `b` może kończyć się dopasowanie.
/// Wynik to koszt znormalizowany długością ścieżki.
fn dtw(a: &[Frame], b: &[Frame], start_slack: usize, end_range: (usize, usize)) -> f32 {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return f32::INFINITY;
    }
    const INF: f32 = f32::INFINITY;
    // cost[j], len[j] dla bieżącego i poprzedniego wiersza.
    let mut prev_cost = vec![INF; m + 1];
    let mut prev_len = vec![0u32; m + 1];
    for j in 0..=start_slack.min(m) {
        prev_cost[j] = 0.0;
    }
    let mut cost = vec![INF; m + 1];
    let mut len = vec![0u32; m + 1];
    for i in 1..=n {
        cost[0] = INF;
        for j in 1..=m {
            let d = dist(&a[i - 1], &b[j - 1]);
            let candidates = [(prev_cost[j - 1], prev_len[j - 1]), (prev_cost[j], prev_len[j]), (cost[j - 1], len[j - 1])];
            let (best_cost, best_len) = candidates
                .iter()
                .copied()
                .filter(|(c, _)| c.is_finite())
                .min_by(|x, y| (x.0 / (x.1 + 1) as f32).total_cmp(&(y.0 / (y.1 + 1) as f32)))
                .unwrap_or((INF, 0));
            cost[j] = best_cost + d;
            len[j] = best_len + 1;
        }
        std::mem::swap(&mut prev_cost, &mut cost);
        std::mem::swap(&mut prev_len, &mut len);
    }
    let (lo, hi) = (end_range.0.clamp(1, m), end_range.1.clamp(1, m));
    (lo..=hi)
        .filter(|&j| prev_cost[j].is_finite())
        .map(|j| prev_cost[j] / prev_len[j].max(1) as f32)
        .fold(INF, f32::min)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Sztuczna „wypowiedź”: sekwencja dźwięków z harmonicznymi (jak samogłoski).
    pub fn synth(pattern: &[(f32, f32)], tempo: f32, noise: f32) -> Vec<f32> {
        let mut out = vec![0.0f32; (RATE as f32 * 0.25) as usize];
        let mut seed = 12345u32;
        for &(freq, secs) in pattern {
            let n = (RATE as f32 * secs * tempo) as usize;
            for i in 0..n {
                let t = i as f32 / RATE as f32;
                let env = ((i as f32 / n as f32) * std::f32::consts::PI).sin();
                let mut v = 0.0;
                for h in 1..=4 {
                    v += (t * freq * h as f32 * std::f32::consts::TAU).sin() / h as f32;
                }
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let r = (seed >> 16) as f32 / 65_536.0 - 0.5;
                out.push(v * env * 0.25 + r * noise);
            }
        }
        out.extend(vec![0.0f32; (RATE as f32 * 0.25) as usize]);
        out
    }

    pub const WAKE: &[(f32, f32)] = &[(220.0, 0.12), (520.0, 0.14), (310.0, 0.12), (700.0, 0.18), (260.0, 0.16)];
    pub const OTHER: &[(f32, f32)] = &[(900.0, 0.2), (150.0, 0.15), (1200.0, 0.2), (400.0, 0.15)];

    fn feats(samples: &[f32]) -> Vec<Frame> {
        trim(&Mfcc::new().features(samples), true)
    }

    #[test]
    fn mfcc_produces_frames_every_10ms() {
        let f = Mfcc::new().features(&vec![0.1; RATE as usize]);
        assert_eq!(f.frames.len(), (16_000 - 400) / 160 + 1);
    }

    #[test]
    fn same_phrase_at_different_tempo_matches_better_than_other_phrase() {
        let template = feats(&synth(WAKE, 1.0, 0.01));
        let faster = feats(&synth(WAKE, 0.85, 0.02));
        let slower = feats(&synth(WAKE, 1.2, 0.02));
        let other = feats(&synth(OTHER, 1.0, 0.02));
        let same_a = dtw_full(&cmn(&template), &cmn(&faster));
        let same_b = dtw_full(&cmn(&template), &cmn(&slower));
        let diff = dtw_full(&cmn(&template), &cmn(&other));
        assert!(same_a < diff * 0.7 && same_b < diff * 0.7, "same {same_a}/{same_b} vs other {diff}");
    }

    #[test]
    fn prefix_match_finds_wake_phrase_followed_by_command() {
        let template = feats(&synth(WAKE, 1.0, 0.01));
        let mut with_command = synth(WAKE, 1.05, 0.02);
        with_command.extend(synth(OTHER, 1.0, 0.02)); // „…, zmień kategorię”
        let start = trim(&Mfcc::new().features(&with_command), false);
        let only_command = trim(&Mfcc::new().features(&synth(OTHER, 1.0, 0.02)), false);
        let hit = dtw_prefix(&template, &start);
        let miss = dtw_prefix(&template, &only_command);
        assert!(hit < miss * 0.75, "hit {hit} vs miss {miss}");
    }
}
