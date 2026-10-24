//! Model emotek oraz współdzielony, lock-free magazyn `EmoteStore`.
//!
//! Każdy otwarty kanał ma własną tabelę emotek; widok `EmoteMap` łączy
//! tabelę kanału z globalną.
//!
//! Odczyty (parser przy każdej wiadomości, UI przy każdej klatce) są
//! znacznie częstsze niż zapisy (pobranie zestawu 7TV). Dlatego używamy
//! `ArcSwap`: czytelnik bierze migawkę bez blokady, a pisarz atomowo
//! podmienia całą mapę.

use std::{collections::HashMap, sync::Arc};

use arc_swap::ArcSwap;
use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmoteProvider {
    Twitch,
    SevenTv,
}

/// Rozdzielczość pobieranych obrazków. `X1` = 28–32 px wysokości,
/// `X2` wygląda ostro na ekranach HiDPI kosztem 4× większych plików.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)] // opcje konfiguracji wybierane w BackendConfig
pub enum EmoteScale {
    #[default]
    X1,
    X2,
    X3,
    X4,
}

impl EmoteScale {
    /// Prefiks nazwy pliku w CDN 7TV, np. `"2x"` → `2x.webp`.
    pub fn seventv_prefix(self) -> &'static str {
        match self {
            Self::X1 => "1x",
            Self::X2 => "2x",
            Self::X3 => "3x",
            Self::X4 => "4x",
        }
    }

    /// Segment ścieżki w CDN Twitcha (maksimum to 3.0).
    pub fn twitch_suffix(self) -> &'static str {
        match self {
            Self::X1 => "1.0",
            Self::X2 => "2.0",
            Self::X3 | Self::X4 => "3.0",
        }
    }

    pub fn factor(self) -> f32 {
        match self {
            Self::X1 => 1.0,
            Self::X2 => 2.0,
            Self::X3 => 3.0,
            Self::X4 => 4.0,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // `id` i `animated` — pod przyszłe funkcje (np. pauzowanie animacji)
pub struct Emote {
    pub id: Arc<str>,
    /// Nazwa wpisywana w czacie (dla 7TV — alias aktywny w danym zestawie).
    pub name: Arc<str>,
    pub url: Arc<str>,
    /// Rozmiar logiczny (punkty UI w skali 1x), niezależny od pobranej rozdzielczości.
    pub width: f32,
    pub height: f32,
    pub animated: bool,
    /// Emotka „nakładka” (np. RainTime, SteerR) — rysowana na poprzedniej.
    pub zero_width: bool,
    pub provider: EmoteProvider,
}

impl Emote {
    /// Natywna emotka Twitcha. `default` w URL-u serwuje GIF dla animowanych
    /// i PNG dla statycznych — loader egui obsługuje oba formaty.
    pub fn twitch(id: &str, code: &str, scale: EmoteScale) -> Self {
        Self {
            id: id.into(),
            name: code.into(),
            url: format!(
                "https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/{}",
                scale.twitch_suffix()
            )
            .into(),
            width: 28.0,
            height: 28.0,
            animated: false,
            zero_width: false,
            provider: EmoteProvider::Twitch,
        }
    }
}

pub type EmoteTable = HashMap<Arc<str>, Arc<Emote>>;

fn build_table(emotes: Vec<Emote>) -> Arc<EmoteTable> {
    Arc::new(
        emotes
            .into_iter()
            .map(|e| (e.name.clone(), Arc::new(e)))
            .collect(),
    )
}

/// Widok emotek dostępnych na jednym kanale: kanałowe + globalne.
/// Dwa `Arc` — sklonowanie widoku to dwie operacje atomowe, bez kopiowania map.
#[derive(Debug, Default, Clone)]
pub struct EmoteMap {
    global: Arc<EmoteTable>,
    channel: Arc<EmoteTable>,
}

impl EmoteMap {
    pub fn new(global: Vec<Emote>, channel: Vec<Emote>) -> Self {
        Self { global: build_table(global), channel: build_table(channel) }
    }

    /// Emotki kanałowe mają pierwszeństwo przed globalnymi (tak jak w kliencie 7TV).
    #[inline]
    pub fn get(&self, name: &str) -> Option<&Arc<Emote>> {
        self.channel.get(name).or_else(|| self.global.get(name))
    }

    /// Wszystkie emotki do wyboru w pickerze i podpowiedziach:
    /// najpierw kanałowe, potem globalne (bez duplikatów nazw), alfabetycznie.
    pub fn sorted_for_picker(&self) -> Vec<Arc<Emote>> {
        let mut channel: Vec<_> = self.channel.values().cloned().collect();
        let mut global: Vec<_> = self
            .global
            .values()
            .filter(|e| !self.channel.contains_key(&e.name))
            .cloned()
            .collect();
        let by_name = |a: &Arc<Emote>, b: &Arc<Emote>| a.name.to_lowercase().cmp(&b.name.to_lowercase());
        channel.sort_by(by_name);
        global.sort_by(by_name);
        channel.extend(global);
        channel
    }
}

/// Stan magazynu: emotki globalne + osobna tabela dla każdego otwartego kanału.
#[derive(Debug, Default, Clone)]
struct EmoteState {
    global: Arc<EmoteTable>,
    /// Klucz: numeryczne ID kanału. Obecność klucza oznacza „kanał otwarty”.
    channels: HashMap<Arc<str>, Arc<EmoteTable>>,
}

#[derive(Default)]
pub struct EmoteStore {
    inner: ArcSwap<EmoteState>,
}

impl EmoteStore {
    /// Widok dla kanału (lock-free). Nieznany kanał → tylko emotki globalne.
    pub fn map_for(&self, channel_id: &str) -> EmoteMap {
        let state = self.inner.load();
        EmoteMap {
            global: state.global.clone(),
            channel: state.channels.get(channel_id).cloned().unwrap_or_default(),
        }
    }

    pub fn set_global(&self, emotes: Vec<Emote>) {
        let global = build_table(emotes);
        self.inner.rcu(|cur| EmoteState { global: global.clone(), channels: cur.channels.clone() });
    }

    /// Rejestruje otwarty kanał (z pustą tabelą do czasu odpowiedzi 7TV).
    pub fn begin_channel(&self, channel_id: &str) {
        let id: Arc<str> = channel_id.into();
        self.inner.rcu(|cur| {
            let mut next = (**cur).clone();
            next.channels.entry(id.clone()).or_default();
            next
        });
    }

    /// Zapisuje emotki kanału tylko wtedy, gdy kanał jest nadal otwarty —
    /// odpowiedź 7TV może przyjść już po zamknięciu zakładki.
    pub fn set_channel(&self, channel_id: &str, emotes: Vec<Emote>) -> bool {
        let table = build_table(emotes);
        let mut applied = false;
        self.inner.rcu(|cur| {
            let mut next = (**cur).clone();
            applied = match next.channels.get_mut(channel_id) {
                Some(slot) => {
                    *slot = table.clone();
                    true
                }
                None => false,
            };
            next
        });
        applied
    }

    pub fn remove_channel(&self, channel_id: &str) {
        self.inner.rcu(|cur| {
            let mut next = (**cur).clone();
            next.channels.remove(channel_id);
            next
        });
    }
}

/// Źródło emotek firm trzecich. Dziś implementuje je tylko 7TV (API v3),
/// ale ten sam trait obsłuży 7TV v4, BTTV czy FFZ bez zmian w reszcie kodu.
#[async_trait]
pub trait EmoteSource: Send + Sync {
    fn name(&self) -> &'static str;
    async fn global_emotes(&self) -> anyhow::Result<Vec<Emote>>;
    /// `twitch_channel_id` to numeryczne ID kanału (z tagu ROOMSTATE).
    async fn channel_emotes(&self, twitch_channel_id: &str) -> anyhow::Result<Vec<Emote>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emote(name: &str) -> Emote {
        Emote {
            id: name.into(),
            name: name.into(),
            url: format!("https://x/{name}").into(),
            width: 32.0,
            height: 32.0,
            animated: false,
            zero_width: false,
            provider: EmoteProvider::SevenTv,
        }
    }

    #[test]
    fn channels_are_isolated_and_closed_channels_reject_late_results() {
        let store = EmoteStore::default();
        store.set_global(vec![emote("KEKW")]);
        store.begin_channel("1");
        store.begin_channel("2");
        assert!(store.set_channel("1", vec![emote("catJAM")]));
        assert!(store.set_channel("2", vec![emote("pepeD")]));

        let one = store.map_for("1");
        assert!(one.get("catJAM").is_some() && one.get("KEKW").is_some());
        assert!(one.get("pepeD").is_none(), "emotki kanału 2 nie mogą wyciec do kanału 1");

        store.remove_channel("2");
        assert!(!store.set_channel("2", vec![emote("late")]), "spóźniona odpowiedź po zamknięciu zakładki");
        assert!(store.map_for("2").get("pepeD").is_none());
    }

    #[test]
    fn picker_lists_channel_emotes_first_without_duplicates() {
        let map = EmoteMap::new(vec![emote("b"), emote("A"), emote("dup")], vec![emote("z"), emote("dup")]);
        let names: Vec<_> = map.sorted_for_picker().iter().map(|e| e.name.to_string()).collect();
        assert_eq!(names, ["dup", "z", "A", "b"]);
    }
}
