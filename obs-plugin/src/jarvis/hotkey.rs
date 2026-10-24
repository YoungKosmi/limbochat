//! Globalny skrót klawiszowy (domyślnie Shift + Z + X) — także jeden
//! klawisz albo boczny przycisk myszy i tryb „przytrzymaj, żeby mówić”.
//!
//! Skrót ma działać także w grze na pełnym ekranie, a kombinacja z dwoma
//! zwykłymi klawiszami (Z + X) nie da się zarejestrować standardowym
//! `RegisterHotKey`. Dlatego co 15 ms sprawdzamy stan klawiszy
//! (`GetAsyncKeyState`) — bez hooków klawiatury, które mogłyby niepokoić
//! systemy anty-cheat. Klawisze dalej trafiają normalnie do gry.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// Nazwy klawiszy jak w panelu: Shift, Ctrl, Alt, A–Z, 0–9, F1–F24,
/// Num0–Num9, Pause, ScrollLock, Insert, Home, End, PageUp, PageDown,
/// Mysz3 (środkowy), Mysz4, Mysz5 (boczne przyciski myszy).
pub fn parse_key(name: &str) -> Option<u16> {
    let upper = name.trim().to_ascii_uppercase();
    match upper.as_str() {
        "SHIFT" => Some(0x10),
        "CTRL" | "CONTROL" => Some(0x11),
        "ALT" => Some(0x12),
        "MYSZ3" => Some(0x04),
        "MYSZ4" => Some(0x05),
        "MYSZ5" => Some(0x06),
        "PAUSE" => Some(0x13),
        "SCROLLLOCK" => Some(0x91),
        "INSERT" => Some(0x2D),
        "HOME" => Some(0x24),
        "END" => Some(0x23),
        "PAGEUP" => Some(0x21),
        "PAGEDOWN" => Some(0x22),
        _ if upper.starts_with("NUM") => {
            let n: u16 = upper[3..].parse().ok()?;
            (n <= 9).then_some(0x60 + n)
        }
        _ => {
            let bytes = upper.as_bytes();
            if bytes.len() == 1 && (bytes[0].is_ascii_uppercase() || bytes[0].is_ascii_digit()) {
                return Some(bytes[0] as u16); // VK dla A–Z i 0–9 = kod ASCII
            }
            let n: u16 = upper.strip_prefix('F')?.parse().ok()?;
            (1..=24).contains(&n).then_some(0x70 + n - 1)
        }
    }
}

/// Klawisze, które mogą działać same (nie przeszkadzają w pisaniu):
/// F1–F24, numeryczne, Pause, ScrollLock, Insert, Home, End, PgUp, PgDn i przyciski myszy.
fn works_alone(vk: u16) -> bool {
    matches!(vk, 0x70..=0x87 | 0x60..=0x69 | 0x04..=0x06 | 0x13 | 0x91 | 0x2D | 0x21..=0x24)
}

/// Skrót: jeden „bezpieczny” klawisz (np. F13, Mysz4) albo 2–4 klawisze,
/// w tym przynajmniej jeden zwykły (nie sam Shift+Ctrl).
pub fn parse_combo(keys: &[String]) -> Option<Vec<u16>> {
    let codes: Vec<u16> = keys.iter().map(|k| parse_key(k)).collect::<Option<_>>()?;
    let has_regular = codes.iter().any(|c| !(0x10..=0x12).contains(c));
    let unique = codes.iter().collect::<std::collections::HashSet<_>>().len() == codes.len();
    let ok = match codes.len() {
        1 => works_alone(codes[0]),
        2..=4 => has_regular && unique,
        _ => false,
    };
    ok.then_some(codes)
}

pub const SUPPORTED: bool = cfg!(windows);

pub struct Hotkey {
    combo: Arc<Mutex<Vec<u16>>>,
    stop: Arc<AtomicBool>,
}

impl Hotkey {
    /// Uruchamia nasłuch. `on_change(true)` = wciśnięty, `false` = puszczony
    /// (wołane w wątku nasłuchu).
    pub fn spawn(combo: Vec<u16>, on_press: impl Fn(bool) + Send + 'static) -> Self {
        let combo = Arc::new(Mutex::new(combo));
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(windows)]
        {
            let (combo, stop) = (combo.clone(), stop.clone());
            std::thread::Builder::new()
                .name("limbochat-hotkey".into())
                .spawn(move || poll(combo, stop, on_press))
                .ok();
        }
        #[cfg(not(windows))]
        drop(on_press);
        Self { combo, stop }
    }

    pub fn set(&self, combo: Vec<u16>) {
        *self.combo.lock().unwrap_or_else(|e| e.into_inner()) = combo;
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(windows)]
fn poll(combo: Arc<Mutex<Vec<u16>>>, stop: Arc<AtomicBool>, on_press: impl Fn(bool)) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    let mut was_down = false;
    while !stop.load(Ordering::Relaxed) {
        let keys = combo.lock().unwrap_or_else(|e| e.into_inner()).clone();
        // SAFETY: GetAsyncKeyState tylko odczytuje stan klawisza.
        let down = !keys.is_empty() && keys.iter().all(|&vk| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0);
        if down != was_down {
            on_press(down);
        }
        was_down = down;
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_combo() {
        let keys: Vec<String> = ["Shift", "Z", "X"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_combo(&keys), Some(vec![0x10, 0x5A, 0x58]));
    }

    #[test]
    fn rejects_bad_combos() {
        let combo = |k: &[&str]| parse_combo(&k.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(combo(&["Shift"]).is_none(), "za krótki");
        assert!(combo(&["Shift", "Ctrl"]).is_none(), "same modyfikatory");
        assert!(combo(&["Z", "Z"]).is_none(), "powtórzony klawisz");
        assert!(combo(&["Ctrl", "Alt", "F13"]).is_some());
        assert!(combo(&["Shift", "Ą"]).is_none());
        assert!(combo(&["Z"]).is_none(), "sama litera przeszkadzałaby w pisaniu");
        assert!(combo(&["Ctrl"]).is_none());
    }

    #[test]
    fn single_push_to_talk_keys() {
        let combo = |k: &[&str]| parse_combo(&k.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(combo(&["F13"]), Some(vec![0x7C]));
        assert_eq!(combo(&["Mysz4"]), Some(vec![0x05]));
        assert_eq!(combo(&["Num0"]), Some(vec![0x60]));
        assert_eq!(combo(&["Pause"]), Some(vec![0x13]));
        assert!(combo(&["Num10"]).is_none());
        assert_eq!(combo(&["Ctrl", "Mysz5"]), Some(vec![0x11, 0x06]));
    }
}
