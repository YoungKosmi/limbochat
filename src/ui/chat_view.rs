//! Wirtualizowana lista wiadomości z emotkami w linii z tekstem.
//!
//! ## Wirtualizacja przy zmiennej wysokości wierszy
//! `ScrollArea::show_rows` wymaga stałej wysokości wiersza, a wiadomości
//! czatu zawijają się na 1–N linii. Dlatego:
//! 1. każdy wiersz pamięta zmierzoną wysokość (`height`), a dopóki nie był
//!    narysowany, używamy średniej z dotychczasowych pomiarów,
//! 2. w każdej klatce liczymy pozycje Y (suma 5000 liczb to ~mikrosekundy)
//!    i rysujemy TYLKO wiersze przecinające viewport,
//! 3. każdy widoczny wiersz rysujemy w osobnym `Ui` w absolutnej pozycji
//!    i zapisujemy jego rzeczywistą wysokość,
//! 4. zmiana szerokości okna unieważnia pomiary (inne zawijanie).
//!
//! ## Tekst + obrazek w jednej linii
//! `ui.horizontal_wrapped` układa widgety od lewej do prawej i zawija do
//! nowej linii, gdy brakuje miejsca — także w środku długiego `Label`.
//! Emotka to `egui::Image` o stałej wysokości wstawiony między etykiety.
//!
//! ## Smaczki
//! * nowe wiadomości wjeżdżają z lewej i pojawiają się płynnie (240 ms),
//! * wzmianki o Tobie mają fioletowe szkło i świecący pasek z lewej,
//! * wiersz pod kursorem delikatnie się podświetla,
//! * pigułka „↓ N nowych” liczy wiadomości, które przyszły, gdy czytasz historię.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use chrono::{DateTime, Local};
use egui::{
    Align, Color32, CornerRadius, Image, Label, Layout, Rect, RichText, ScrollArea, Sense, Shape, Stroke,
    Ui, UiBuilder, Vec2, pos2, vec2,
};

use super::theme::{self, alpha, palette::*};
use crate::{
    emotes::{Emote, EmoteMap, EmoteScale},
    events::Command,
    helix::BadgeMap,
    parser::{self, ParsedMessage, Segment},
    ui::mod_menu,
};

/// Maksymalna wysokość pojedynczego wiersza (zabezpieczenie przed spamem
/// tysiącami znaków bez spacji).
const MAX_ROW_HEIGHT: f32 = 2_000.0;
const APPEAR: Duration = Duration::from_millis(240);
/// Okno, z którego liczymy tempo czatu (wiadomości na sekundę).
const RATE_WINDOW: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemLevel {
    Info,
    Error,
}

pub enum RowKind {
    Chat(Box<ParsedMessage>),
    System { text: String, level: SystemLevel, time: DateTime<Local> },
}

pub struct Row {
    /// Stabilny identyfikator — źródło `Id` widgetów w wierszu, niezależny
    /// od pozycji w kolejce (która zmienia się po usunięciu najstarszych).
    seq: u64,
    kind: RowKind,
    height: Option<f32>,
    born: Instant,
}

/// Opcje rysowania jednej zakładki, budowane w każdej klatce.
pub struct RenderOptions<'a> {
    /// Użytkownik chce widzieć szybkie przyciski moderacji.
    pub mod_mode: bool,
    /// Zalogowany użytkownik ma uprawnienia moderatora na tym kanale.
    pub can_moderate: bool,
    pub emote_height: f32,
    pub show_timestamps: bool,
    pub animate: bool,
    /// Login zalogowanego użytkownika (małymi literami) — do podświetlania wzmianek.
    pub my_login: Option<&'a str>,
    /// Odznaki globalne + kanałowe (kanałowe nadpisują globalne).
    pub badges: &'a BadgeMap,
}

pub struct ChatView {
    rows: VecDeque<Row>,
    max_rows: usize,
    next_seq: u64,
    last_width: f32,
    height_estimate: f32,
    at_bottom: bool,
    scroll_to_bottom: bool,
    /// Wiadomości, które przyszły, gdy użytkownik czytał historię.
    unseen: usize,
    recent: VecDeque<Instant>,
}

impl ChatView {
    pub fn new(max_rows: usize) -> Self {
        Self {
            rows: VecDeque::with_capacity(max_rows),
            max_rows,
            next_seq: 0,
            last_width: 0.0,
            height_estimate: 28.0,
            at_bottom: true,
            scroll_to_bottom: false,
            unseen: 0,
            recent: VecDeque::new(),
        }
    }

    pub fn is_at_bottom(&self) -> bool {
        self.at_bottom
    }

    /// Tempo czatu w wiadomościach na sekundę (ostatnie 10 s).
    pub fn rate(&mut self) -> f32 {
        let now = Instant::now();
        while self.recent.front().is_some_and(|t| now.duration_since(*t) > RATE_WINDOW) {
            self.recent.pop_front();
        }
        self.recent.len() as f32 / RATE_WINDOW.as_secs_f32()
    }

    fn push(&mut self, kind: RowKind) {
        self.rows.push_back(Row { seq: self.next_seq, kind, height: None, born: Instant::now() });
        self.next_seq += 1;

        // Gdy użytkownik przewinął w górę, nie usuwamy najstarszych wierszy —
        // inaczej czytany fragment „uciekałby” w górę ekranu. Twardy limit
        // (2×) chroni pamięć, gdy ktoś zostawi czat przewinięty na długo.
        let limit = if self.at_bottom { self.max_rows } else { self.max_rows * 2 };
        while self.rows.len() > limit {
            self.rows.pop_front();
        }
    }

    pub fn push_message(&mut self, msg: Box<ParsedMessage>) {
        self.recent.push_back(Instant::now());
        if self.recent.len() > 2_000 {
            self.recent.pop_front();
        }
        if !self.at_bottom {
            self.unseen += 1;
        }
        self.push(RowKind::Chat(msg));
    }

    pub fn push_system(&mut self, text: impl Into<String>, level: SystemLevel) {
        self.push(RowKind::System { text: text.into(), level, time: Local::now() });
    }

    fn chat_rows_mut(&mut self) -> impl DoubleEndedIterator<Item = (&mut ParsedMessage, &mut Option<f32>)> {
        self.rows.iter_mut().filter_map(|r| match &mut r.kind {
            RowKind::Chat(m) => Some((m.as_mut(), &mut r.height)),
            RowKind::System { .. } => None,
        })
    }

    /// Nicki ostatnio piszących (od najnowszych, bez powtórzeń) — do podpowiedzi `@nick`.
    pub fn recent_chatters(&self, limit: usize) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.rows
            .iter()
            .rev()
            .filter_map(|r| match &r.kind {
                RowKind::Chat(m) => Some(&m.sender_name),
                RowKind::System { .. } => None,
            })
            .filter(|name| seen.insert(name.as_str()))
            .take(limit)
            .cloned()
            .collect()
    }

    /// CLEARMSG — świeże wiadomości są na końcu, więc szukamy od tyłu.
    pub fn mark_message_deleted(&mut self, message_id: &str) {
        if let Some((msg, _)) = self.chat_rows_mut().rev().find(|(m, _)| m.id == message_id) {
            msg.deleted = true;
        }
    }

    /// CLEARCHAT dla użytkownika — wyszarzamy wszystkie jego wiadomości.
    pub fn mark_user_cleared(&mut self, user_id: &str) {
        for (msg, _) in self.chat_rows_mut().filter(|(m, _)| m.sender_id == user_id) {
            msg.deleted = true;
        }
    }

    pub fn mark_all_deleted(&mut self) {
        for (msg, _) in self.chat_rows_mut() {
            msg.deleted = true;
        }
    }

    /// Po załadowaniu emotek 7TV uzupełniamy je w już wyświetlonych wiadomościach.
    pub fn retokenize(&mut self, emotes: &EmoteMap, scale: EmoteScale) {
        for (msg, height) in self.chat_rows_mut() {
            if parser::retokenize(&mut msg.segments, emotes, scale) {
                *height = None;
            }
        }
    }

    /// Rysuje czat. Zwraca akcje moderatorskie kliknięte w tej klatce.
    pub fn show(&mut self, ui: &mut Ui, opts: &RenderOptions<'_>) -> Vec<Command> {
        let mut actions = Vec::new();
        let spacing = 2.0;

        let mut scroll = ScrollArea::vertical().auto_shrink(false).stick_to_bottom(true);
        if std::mem::take(&mut self.scroll_to_bottom) {
            scroll = scroll.vertical_scroll_offset(f32::MAX);
        }

        let now = Instant::now();
        let output = scroll.show_viewport(ui, |ui, viewport| {
            let width = ui.available_width();
            if (width - self.last_width).abs() > 0.5 {
                for row in &mut self.rows {
                    row.height = None;
                }
                self.last_width = width;
            }

            let origin = ui.max_rect().min;
            let mut y = 0.0_f32;
            let mut sum_measured = 0.0_f32;
            let mut n_measured = 0_u32;
            let mut animating = false;

            for row in self.rows.iter_mut() {
                let top = y;
                let estimated = row.height.unwrap_or(self.height_estimate);

                if top + estimated >= viewport.min.y && top <= viewport.max.y {
                    // Wjazd nowej wiadomości: przesunięcie z lewej + narastająca nieprzezroczystość.
                    let progress = if opts.animate {
                        (now.duration_since(row.born).as_secs_f32() / APPEAR.as_secs_f32()).min(1.0)
                    } else {
                        1.0
                    };
                    let eased = 1.0 - (1.0 - progress).powi(3);
                    animating |= progress < 1.0;

                    let pad = vec2(8.0, 3.0);
                    let rect = Rect::from_min_size(
                        origin + vec2(pad.x - (1.0 - eased) * 18.0, top + pad.y),
                        vec2(width - pad.x * 2.0, MAX_ROW_HEIGHT),
                    );
                    let mut row_ui = ui.new_child(
                        UiBuilder::new().id_salt(row.seq).max_rect(rect).layout(Layout::top_down(Align::Min)),
                    );
                    if eased < 1.0 {
                        row_ui.multiply_opacity(eased);
                    }
                    let background = row_ui.painter().add(Shape::Noop);
                    let highlight = render_row(&mut row_ui, &row.kind, opts, &mut actions);

                    let content = row_ui.min_rect();
                    let measured = content.height() + pad.y * 2.0;
                    let bg_rect = Rect::from_min_max(
                        pos2(origin.x + 4.0, content.top() - pad.y + 1.0),
                        pos2(origin.x + width - 4.0, content.bottom() + pad.y - 1.0),
                    );
                    let hovered = ui.rect_contains_pointer(bg_rect);
                    row_ui.painter().set(background, row_background(bg_rect, highlight, hovered));

                    if row.height.is_none_or(|h| (h - measured).abs() > 0.5) {
                        // Wysokość się zmieniła → pozycje niżej też; dorysuj klatkę.
                        ui.ctx().request_repaint();
                    }
                    row.height = Some(measured);
                    sum_measured += measured;
                    n_measured += 1;
                }

                y += row.height.unwrap_or(self.height_estimate) + spacing;
            }

            if n_measured > 0 {
                let avg = sum_measured / n_measured as f32;
                self.height_estimate = self.height_estimate * 0.9 + avg * 0.1;
            }
            if animating {
                ui.ctx().request_repaint();
            }

            // Rezerwujemy pełną wysokość treści, żeby pasek przewijania był poprawny.
            ui.set_min_size(vec2(width, y + 6.0));
        });

        let bottom_gap = output.content_size.y - (output.state.offset.y + output.inner_rect.height());
        self.at_bottom = bottom_gap <= 4.0;
        if self.at_bottom {
            self.unseen = 0;
        } else {
            self.jump_to_bottom_pill(ui, output.inner_rect, opts.animate);
        }

        actions
    }

    fn jump_to_bottom_pill(&mut self, ui: &mut Ui, area: Rect, animate: bool) {
        let text = match self.unseen {
            0 => "↓  Najnowsze".to_owned(),
            1 => "↓  1 nowa wiadomość".to_owned(),
            n @ 2..=4 => format!("↓  {n} nowe wiadomości"),
            n => format!("↓  {n} nowych wiadomości"),
        };
        let galley = ui.painter().layout_no_wrap(text, theme::semi(13.0), TEXT);
        let size = galley.size() + vec2(28.0, 14.0);
        let rect = Rect::from_center_size(area.center_bottom() - vec2(0.0, size.y * 0.5 + 14.0), size);
        let response = ui.interact(rect, ui.id().with("jump_to_bottom"), Sense::click());

        let hover = ui.ctx().animate_bool_with_time(response.id, response.hovered(), 0.15);
        // Delikatne „unoszenie się” pigułki, gdy czekają nowe wiadomości.
        let bob = if animate && self.unseen > 0 { (ui.input(|i| i.time) as f32 * 3.0).sin() * 1.5 } else { 0.0 };
        let rect = rect.translate(vec2(0.0, bob));
        let radius = rect.height() / 2.0;
        let painter = ui.painter();
        let glow_strength = if self.unseen > 0 { 0.45 } else { 0.2 } + 0.25 * hover;
        let glow = egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: alpha(VIOLET, glow_strength) };
        painter.add(glow.as_shape(rect, radius));
        painter.rect_filled(rect, radius, Color32::from_rgba_premultiplied(30, 24, 48, 235));
        painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, alpha(VIOLET_SOFT, 0.5 + 0.3 * hover)), egui::StrokeKind::Inside);
        painter.galley(rect.center() - galley.size() / 2.0, galley, TEXT);

        if response.clicked() {
            self.scroll_to_bottom = true;
        }
        if animate && self.unseen > 0 {
            ui.ctx().request_repaint();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Highlight {
    None,
    Mention,
    Error,
}

fn row_background(rect: Rect, highlight: Highlight, hovered: bool) -> Shape {
    let radius = CornerRadius::same(10);
    match highlight {
        Highlight::Mention => {
            let bar = Rect::from_min_max(rect.min + vec2(0.0, 3.0), pos2(rect.left() + 3.0, rect.bottom() - 3.0));
            let glow = egui::Shadow { offset: [0, 0], blur: 10, spread: 0, color: alpha(ORCHID, 0.6) };
            Shape::Vec(vec![
                Shape::rect_filled(rect, radius, alpha(VIOLET, if hovered { 0.22 } else { 0.16 })),
                Shape::rect_stroke(rect, radius, Stroke::new(1.0_f32, alpha(VIOLET_SOFT, 0.18)), egui::StrokeKind::Inside),
                glow.as_shape(bar, 1.5).into(),
                Shape::rect_filled(bar, 1.5, ORCHID),
            ])
        }
        Highlight::Error => Shape::rect_filled(rect, radius, alpha(ERR, 0.1)),
        Highlight::None if hovered => Shape::rect_filled(rect, radius, white(9)),
        Highlight::None => Shape::Noop,
    }
}

fn render_row(ui: &mut Ui, kind: &RowKind, opts: &RenderOptions<'_>, actions: &mut Vec<Command>) -> Highlight {
    match kind {
        RowKind::Chat(msg) => render_chat_message(ui, msg, opts, actions),
        RowKind::System { text, level, time } => {
            ui.horizontal_wrapped(|ui| {
                if opts.show_timestamps {
                    ui.label(RichText::new(time.format("%H:%M").to_string()).font(theme::mono(11.0)).color(TEXT_FAINT));
                }
                let (marker, color) = match level {
                    SystemLevel::Info => ("◆", alpha(VIOLET_SOFT, 0.7)),
                    SystemLevel::Error => ("▲", ERR),
                };
                ui.label(RichText::new(marker).size(9.0).color(color));
                let text_color = match level {
                    SystemLevel::Info => TEXT_DIM,
                    SystemLevel::Error => ERR,
                };
                ui.label(RichText::new(text).size(13.0).color(text_color));
            });
            if *level == SystemLevel::Error { Highlight::Error } else { Highlight::None }
        }
    }
}

fn mentions(msg: &ParsedMessage, me: Option<&str>) -> bool {
    let Some(me) = me else { return false };
    msg.sender_login != me
        && msg
            .raw_text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| word.eq_ignore_ascii_case(me))
}

fn render_chat_message(ui: &mut Ui, msg: &ParsedMessage, opts: &RenderOptions<'_>, actions: &mut Vec<Command>) -> Highlight {
    let highlight = if mentions(msg, opts.my_login) { Highlight::Mention } else { Highlight::None };

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        // Wysokość linii = wysokość emotki → tekst i obrazki są wyrównane.
        ui.set_row_height(opts.emote_height);

        if msg.deleted {
            ui.multiply_opacity(0.38);
        }

        if opts.mod_mode && opts.can_moderate {
            mod_menu::quick_actions(ui, msg, actions);
        }

        if opts.show_timestamps {
            let local = msg.timestamp.with_timezone(&Local);
            ui.label(RichText::new(local.format("%H:%M").to_string()).font(theme::mono(11.0)).color(TEXT_FAINT));
        }

        for badge in &msg.badges {
            let key = format!("{}/{}", badge.name, badge.version);
            if let Some(image) = opts.badges.get(&key) {
                ui.add(Image::new(image.url.as_ref()).fit_to_exact_size(vec2(18.0, 18.0)).corner_radius(4.0))
                    .on_hover_text(&image.title);
            } else if let Some((text, color)) = badge_chip(&badge.name) {
                badge_tag(ui, text, color).on_hover_text(&badge.name);
            }
        }

        let name_color = readable_color(msg.color, &msg.sender_login);
        let name = ui
            .add(Label::new(RichText::new(&msg.sender_name).font(theme::semi(14.5)).color(name_color)).sense(Sense::click()))
            .on_hover_text("Prawy przycisk: akcje");
        name.context_menu(|ui| mod_menu::user_context_menu(ui, msg, opts.can_moderate, actions));

        if !msg.is_action {
            ui.label(RichText::new(":").color(TEXT_FAINT));
        }

        // `/me` — cała wiadomość w kolorze nicka i kursywą, jak na Twitchu.
        let text_color = if msg.is_action { name_color } else { TEXT };

        for segment in &msg.segments {
            match segment {
                Segment::Text(text) => {
                    let mut rt = RichText::new(text).color(text_color);
                    if msg.is_action {
                        rt = rt.italics();
                    }
                    if msg.deleted {
                        rt = rt.strikethrough();
                    }
                    ui.label(rt);
                }
                Segment::Emote(e) => {
                    let size = emote_size(&e.base, opts.emote_height);
                    let response = ui
                        .add(Image::new(e.base.url.as_ref()).fit_to_exact_size(size))
                        .on_hover_ui(|ui| emote_tooltip(ui, &e.base, opts.emote_height));

                    for overlay in &e.overlays {
                        let osize = emote_size(overlay, opts.emote_height);
                        let orect = Rect::from_center_size(response.rect.center(), osize);
                        Image::new(overlay.url.as_ref()).paint_at(ui, orect);
                    }
                }
                Segment::Mention(m) => {
                    // Wzmianka jako mała szklana pigułka.
                    let is_me = opts.my_login.is_some_and(|me| m.trim_start_matches('@').eq_ignore_ascii_case(me));
                    let color = if is_me { ORCHID } else { VIOLET_SOFT };
                    ui.label(RichText::new(m).font(theme::semi(14.0)).color(color).background_color(alpha(color, 0.12)));
                }
                Segment::Link(url) => {
                    ui.hyperlink_to(RichText::new(url).color(VIOLET_SOFT).underline(), crate::parser::link_url(url));
                }
            }
        }
    });

    highlight
}

fn badge_tag(ui: &mut Ui, text: &str, color: Color32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), theme::mono(9.5), Color32::WHITE);
    let size = galley.size() + vec2(8.0, 4.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 4.0, alpha(color, 0.85));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    response
}

/// Rozmiar emotki przy stałej wysokości linii, z zachowaniem proporcji
/// (szerokie emotki 7TV, np. 3:1, nie są ściskane).
fn emote_size(emote: &Emote, line_height: f32) -> Vec2 {
    let aspect = if emote.height > 0.0 { emote.width / emote.height } else { 1.0 };
    vec2(line_height * aspect, line_height)
}

fn emote_tooltip(ui: &mut Ui, emote: &Emote, line_height: f32) {
    ui.vertical_centered(|ui| {
        ui.add(Image::new(emote.url.as_ref()).fit_to_exact_size(emote_size(emote, line_height * 3.0)));
        ui.label(RichText::new(emote.name.as_ref()).font(theme::semi(14.0)));
        let provider = match emote.provider {
            crate::emotes::EmoteProvider::Twitch => "Twitch",
            crate::emotes::EmoteProvider::SevenTv => "7TV",
        };
        let kind = if emote.zero_width { " · zero-width" } else { "" };
        ui.label(theme::tag(&format!("{provider}{kind}"), TEXT_FAINT));
    });
}

/// Tekstowe odznaki — gdy obrazki z Helix nie są dostępne (niezalogowany).
/// Kolory przygaszone, żeby pasowały do matowej palety.
fn badge_chip(name: &str) -> Option<(&'static str, Color32)> {
    Some(match name {
        "broadcaster" => ("LIVE", Color32::from_rgb(0xc8, 0x4b, 0x5a)),
        "moderator" => ("MOD", Color32::from_rgb(0x3f, 0x9e, 0x6e)),
        "vip" => ("VIP", Color32::from_rgb(0xb4, 0x5c, 0xc0)),
        "subscriber" | "founder" => ("SUB", VIOLET),
        "staff" | "admin" | "global_mod" => ("STAFF", Color32::from_rgb(0x55, 0x55, 0x66)),
        _ => return None,
    })
}

/// Kolory nicków wybierane przez użytkowników bywają nieczytelne na ciemnym
/// tle (np. #0000FF). Za ciemne rozjaśniamy; brak koloru → stały kolor z hasha.
fn readable_color(color: Option<[u8; 3]>, login: &str) -> Color32 {
    const FALLBACK: [[u8; 3]; 8] = [
        [0xff, 0x9e, 0x80], [0x8c, 0xb4, 0xff], [0x7d, 0xd3, 0xa8], [0xf0, 0xc9, 0x7a],
        [0xe8, 0x9a, 0x6e], [0x7f, 0xc8, 0xd0], [0xf2, 0x9c, 0xc8], [0xb9, 0xa6, 0xf7],
    ];
    let [r, g, b] = color.unwrap_or_else(|| {
        let hash = login.bytes().fold(0_usize, |h, c| h.wrapping_mul(31).wrapping_add(c as usize));
        FALLBACK[hash % FALLBACK.len()]
    });

    let luminance = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
    if luminance < 0.4 {
        let lift = |c: u8| (c as f32 + (255.0 - c as f32) * 0.5) as u8;
        Color32::from_rgb(lift(r), lift(g), lift(b))
    } else {
        Color32::from_rgb(r, g, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(sender: &str, text: &str) -> ParsedMessage {
        ParsedMessage {
            id: "1".into(),
            channel_login: "c".into(),
            channel_id: "1".into(),
            sender_id: "2".into(),
            sender_login: sender.into(),
            sender_name: sender.into(),
            color: None,
            badges: Default::default(),
            is_action: false,
            timestamp: chrono::Utc::now(),
            segments: Default::default(),
            raw_text: text.into(),
            deleted: false,
        }
    }

    #[test]
    fn mention_detection_matches_whole_words_only() {
        assert!(mentions(&msg("ktos", "hej @Limbo co tam"), Some("limbo")));
        assert!(mentions(&msg("ktos", "limbo, patrz"), Some("limbo")));
        assert!(!mentions(&msg("ktos", "limbochat jest super"), Some("limbo")), "fragment słowa to nie wzmianka");
        assert!(!mentions(&msg("limbo", "ja limbo"), Some("limbo")), "własne wiadomości nie są wzmianką");
    }
}
