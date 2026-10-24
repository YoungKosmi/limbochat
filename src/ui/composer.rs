//! Pole wiadomości z obsługą emotek 7TV.
//!
//! Wysłanie emotki 7TV to po prostu wysłanie jej NAZWY jako tekstu — każdy,
//! kto ma 7TV (rozszerzenie lub klient jak LiMBOCHAT), zobaczy obrazek.
//! Dlatego cała praca polega na wygodnym wpisywaniu nazw:
//! * **Tab** — uzupełnia słowo przed kursorem; kolejne Taby przełączają
//!   między dopasowaniami. `@nick` uzupełnia nicki z czatu.
//! * **Picker** (przycisk 😀) — siatka emotek z wyszukiwarką; kliknięcie
//!   wstawia nazwę do pola. Rysowane są tylko widoczne wiersze, więc
//!   obrazki pobierają się dopiero przy przewijaniu.

use std::sync::Arc;

use egui::{
    Align2, Id, Image, Key, Modifiers, RichText, ScrollArea, Sense, TextEdit, Ui, vec2,
    text::{CCursor, CCursorRange},
};

use super::theme;
use crate::{emotes::Emote, helix::MAX_MESSAGE_CHARS};

const PICKER_CELL: f32 = 38.0;
const MAX_COMPLETIONS: usize = 50;

#[derive(Default)]
pub struct Composer {
    text: String,
    completion: Option<Completion>,
    picker_open: bool,
    picker_filter: String,
    focused: bool,
}

/// Stan cyklicznego uzupełniania (kolejne Taby).
struct Completion {
    /// Tekst przed uzupełnianym słowem.
    base: String,
    candidates: Vec<String>,
    index: usize,
    /// Tekst pola po ostatnim uzupełnieniu — jeśli użytkownik coś zmienił,
    /// następny Tab zaczyna nowe wyszukiwanie.
    applied: String,
}

impl Composer {
    /// Rysuje pole. Zwraca tekst do wysłania, jeśli użytkownik go zatwierdził.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        enabled: bool,
        hint: &str,
        emotes: &[Arc<Emote>],
        chatters: impl FnOnce() -> Vec<String>,
    ) -> Option<String> {
        let input_id = ui.make_persistent_id("composer_input");
        let mut to_send = None;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let picker = emote_button(ui, enabled, self.picker_open).on_hover_text("Emotki 7TV");
            if picker.clicked() {
                self.picker_open = !self.picker_open;
            }

            // Tab przechwytujemy, zanim zobaczy go pole tekstowe.
            let focused = ui.memory(|m| m.has_focus(input_id));
            if focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
                if self.complete(emotes, chatters) {
                    move_cursor_to_end(ui, input_id, &self.text);
                }
            }

            // Licznik znaków pojawia się dopiero blisko limitu.
            let chars = self.text.chars().count();
            let counter_width = if chars > MAX_MESSAGE_CHARS - 100 { 58.0 } else { 0.0 };
            let send_width = 92.0 + counter_width;
            let response = ui.add_enabled(
                enabled,
                TextEdit::singleline(&mut self.text)
                    .id(input_id)
                    .hint_text(RichText::new(hint).color(theme::palette::TEXT_FAINT))
                    .char_limit(MAX_MESSAGE_CHARS)
                    .lock_focus(true)
                    .frame(false)
                    .font(egui::FontId::proportional(15.0))
                    .margin(vec2(4.0, 6.0))
                    .desired_width((ui.available_width() - send_width).max(80.0)),
            );
            if response.changed() {
                self.completion = None;
            }
            self.focused = response.has_focus();

            if counter_width > 0.0 {
                let color = if chars >= MAX_MESSAGE_CHARS { theme::palette::ERR } else { theme::palette::TEXT_FAINT };
                ui.label(RichText::new(format!("{chars}/{MAX_MESSAGE_CHARS}")).font(theme::mono(11.0)).color(color));
            }

            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let has_text = !self.text.trim().is_empty();
            let clicked = theme::accent_button(ui, "Wyślij", enabled && has_text).clicked();

            if enabled && has_text && (enter || clicked) {
                to_send = Some(std::mem::take(&mut self.text));
                self.completion = None;
                // Po wysłaniu zostajemy w polu — można od razu pisać dalej.
                ui.memory_mut(|m| m.request_focus(input_id));
            }
        });

        if self.picker_open && enabled {
            self.picker(ui, emotes, input_id);
        }
        to_send
    }

    /// Czy pole wiadomości ma fokus (aplikacja rysuje wtedy poświatę wokół pigułki).
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Uzupełnia ostatnie słowo. Zwraca `true`, jeśli tekst się zmienił.
    fn complete(&mut self, emotes: &[Arc<Emote>], chatters: impl FnOnce() -> Vec<String>) -> bool {
        // Kolejny Tab bez zmian w tekście → następne dopasowanie.
        if let Some(c) = &mut self.completion {
            if self.text == c.applied && !c.candidates.is_empty() {
                c.index = (c.index + 1) % c.candidates.len();
                self.text = format!("{}{} ", c.base, c.candidates[c.index]);
                c.applied = self.text.clone();
                return true;
            }
        }

        let split = self.text.rfind(' ').map_or(0, |i| i + 1);
        let (base, word) = self.text.split_at(split);
        if word.is_empty() {
            return false;
        }
        let candidates = completion_candidates(word, emotes, chatters);
        let Some(first) = candidates.first() else { return false };

        let base = base.to_owned();
        self.text = format!("{base}{first} ");
        self.completion = Some(Completion { base, candidates, index: 0, applied: self.text.clone() });
        true
    }

    fn insert_emote(&mut self, name: &str) {
        if !self.text.is_empty() && !self.text.ends_with(' ') {
            self.text.push(' ');
        }
        self.text.push_str(name);
        self.text.push(' ');
        self.completion = None;
    }

    fn picker(&mut self, ui: &mut Ui, emotes: &[Arc<Emote>], input_id: Id) {
        let mut open = self.picker_open;
        let mut picked: Option<Arc<str>> = None;

        egui::Window::new(RichText::new("Emotki 7TV").font(theme::brand(17.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([360.0, 320.0])
            .anchor(Align2::LEFT_BOTTOM, [8.0, -72.0])
            .show(ui.ctx(), |ui| {
                ui.add(
                    TextEdit::singleline(&mut self.picker_filter)
                        .hint_text("🔍 Szukaj emotki…")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);

                let filter = self.picker_filter.to_lowercase();
                let filtered: Vec<&Arc<Emote>> = emotes
                    .iter()
                    .filter(|e| filter.is_empty() || e.name.to_lowercase().contains(&filter))
                    .collect();

                if filtered.is_empty() {
                    let text = if emotes.is_empty() {
                        "Brak emotek 7TV — trwa ładowanie albo kanał nie ma zestawu."
                    } else {
                        "Nic nie pasuje do wyszukiwania."
                    };
                    ui.label(RichText::new(text).weak());
                    return;
                }

                let columns = ((ui.available_width() / PICKER_CELL).floor() as usize).max(1);
                let rows = filtered.len().div_ceil(columns);
                ui.spacing_mut().item_spacing = vec2(2.0, 2.0);

                // Wirtualizacja: tylko widoczne wiersze → tylko ich obrazki są pobierane.
                ScrollArea::vertical().auto_shrink(false).show_rows(ui, PICKER_CELL, rows, |ui, range| {
                    for row in range {
                        ui.horizontal(|ui| {
                            for emote in filtered.iter().skip(row * columns).take(columns) {
                                if emote_cell(ui, emote).clicked() {
                                    picked = Some(emote.name.clone());
                                }
                            }
                        });
                    }
                });
            });

        if let Some(name) = picked {
            self.insert_emote(&name);
            ui.memory_mut(|m| m.request_focus(input_id));
            move_cursor_to_end(ui, input_id, &self.text);
        }
        self.picker_open = open;
    }
}

/// Dopasowania dla słowa: `@prefix` → nicki z czatu; w przeciwnym razie
/// emotki (kanałowe przed globalnymi), a po nich nicki. Wielkość liter
/// nie ma znaczenia przy dopasowaniu, ale wstawiamy oryginalną pisownię
/// (nazwy emotek są wrażliwe na wielkość liter).
fn completion_candidates(word: &str, emotes: &[Arc<Emote>], chatters: impl FnOnce() -> Vec<String>) -> Vec<String> {
    let lower = word.to_lowercase();
    let mut out: Vec<String> = Vec::new();

    if let Some(prefix) = lower.strip_prefix('@') {
        out.extend(
            chatters()
                .into_iter()
                .filter(|c| c.to_lowercase().starts_with(prefix))
                .map(|c| format!("@{c}")),
        );
    } else {
        out.extend(
            emotes
                .iter()
                .filter(|e| e.name.to_lowercase().starts_with(&lower))
                .map(|e| e.name.to_string()),
        );
        out.extend(chatters().into_iter().filter(|c| c.to_lowercase().starts_with(&lower)));
    }

    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert(c.clone()));
    out.truncate(MAX_COMPLETIONS);
    out
}

/// Przycisk pickera: wektorowa buźka, która „uśmiecha się szerzej” przy
/// najechaniu i świeci na fioletowo, gdy picker jest otwarty.
fn emote_button(ui: &mut Ui, enabled: bool, open: bool) -> egui::Response {
    use theme::palette::*;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), sense);
    let hover = ui.ctx().animate_bool_with_time(response.id, enabled && response.hovered(), 0.15);
    let active = ui.ctx().animate_bool_with_time(response.id.with("open"), open, 0.2);
    let painter = ui.painter();
    let c = rect.center();
    if hover + active > 0.0 {
        painter.circle_filled(c, 15.0, theme::alpha(VIOLET, 0.18 * hover + 0.3 * active));
    }
    let color = if !enabled {
        TEXT_FAINT
    } else {
        TEXT_DIM.lerp_to_gamma(VIOLET_SOFT, hover.max(active))
    };
    let stroke = egui::Stroke::new(1.6_f32, color);
    painter.circle_stroke(c, 8.5, stroke);
    painter.circle_filled(c + vec2(-3.0, -2.2), 1.3, color);
    painter.circle_filled(c + vec2(3.0, -2.2), 1.3, color);
    let smile: Vec<egui::Pos2> = (0..=12)
        .map(|i| {
            let t = std::f32::consts::PI * (0.15 + 0.7 * i as f32 / 12.0);
            c + vec2(t.cos() * 4.2, 1.2 + t.sin() * (2.6 + 1.2 * hover))
        })
        .collect();
    painter.add(egui::Shape::line(smile, stroke));
    response
}

/// Komórka pickera: emotka w pełnym rozmiarze (z zachowaniem proporcji)
/// i podświetlenie tła pod kursorem.
fn emote_cell(ui: &mut Ui, emote: &Emote) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(PICKER_CELL, PICKER_CELL), Sense::click());
    // Smaczek: emotka „wyskakuje” (powiększa się) pod kursorem.
    let hover = ui.ctx().animate_bool_with_time(response.id, response.hovered(), 0.12);
    if hover > 0.0 {
        ui.painter().rect_filled(rect, 10.0, theme::alpha(theme::palette::VIOLET, 0.28 * hover));
    }
    let max = vec2(PICKER_CELL - 6.0, 28.0) * (1.0 + 0.22 * hover);
    let aspect = if emote.height > 0.0 { emote.width / emote.height } else { 1.0 };
    let size = if aspect * max.y > max.x { vec2(max.x, max.x / aspect) } else { vec2(max.y * aspect, max.y) };
    Image::new(emote.url.as_ref()).paint_at(ui, egui::Rect::from_center_size(rect.center(), size));
    response.on_hover_text(emote.name.as_ref())
}

fn move_cursor_to_end(ui: &Ui, id: Id, text: &str) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
        let end = CCursor::new(text.chars().count());
        state.cursor.set_char_range(Some(CCursorRange::one(end)));
        state.store(ui.ctx(), id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emotes::EmoteProvider;

    fn emote(name: &str) -> Arc<Emote> {
        Arc::new(Emote {
            id: name.into(),
            name: name.into(),
            url: "x".into(),
            width: 32.0,
            height: 32.0,
            animated: false,
            zero_width: false,
            provider: EmoteProvider::SevenTv,
        })
    }

    #[test]
    fn tab_cycles_through_matches_and_keeps_original_case() {
        let emotes = [emote("catJAM"), emote("catKISS"), emote("KEKW")];
        let mut c = Composer { text: "hej CAT".into(), ..Default::default() };

        assert!(c.complete(&emotes, Vec::new));
        assert_eq!(c.text, "hej catJAM ");
        assert!(c.complete(&emotes, Vec::new));
        assert_eq!(c.text, "hej catKISS ");
        assert!(c.complete(&emotes, Vec::new));
        assert_eq!(c.text, "hej catJAM ", "po ostatnim dopasowaniu wracamy do pierwszego");
    }

    #[test]
    fn at_prefix_completes_chatters_only() {
        let emotes = [emote("modCheck")];
        let mut c = Composer { text: "@mo".into(), ..Default::default() };
        assert!(c.complete(&emotes, || vec!["ModZiomek".into(), "widz".into()]));
        assert_eq!(c.text, "@ModZiomek ");
    }

    #[test]
    fn picker_inserts_name_with_spaces() {
        let mut c = Composer { text: "siema".into(), ..Default::default() };
        c.insert_emote("KEKW");
        assert_eq!(c.text, "siema KEKW ");
    }
}
