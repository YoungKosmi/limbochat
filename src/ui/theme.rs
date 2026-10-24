//! Motyw LiMBOCHAT: matowy fiolet, czerń i biel w stylu „liquid glass”.
//!
//! egui nie potrafi rozmywać tła pod panelem (brak backdrop-filter), więc
//! efekt szkła budujemy z warstw:
//! 1. **Tło** — animowana zorza z miękkich gradientów radialnych (siatki
//!    z kolorowanymi wierzchołkami — gładkie i tanie dla GPU) i drobne
//!    ziarno, które daje matowy, „papierowy” charakter.
//! 2. **Szkło** — półprzezroczyste panele z cienką jasną krawędzią,
//!    połyskiem przy górnej krawędzi i miękkim cieniem.
//! 3. **Opcjonalnie Mica** (Windows 11) — system rozmywa pulpit pod oknem,
//!    a my rysujemy zorzę półprzezroczyście na wierzchu.
//!
//! Animacje działają tylko wtedy, gdy okno jest aktywne, i można je
//! wyłączyć przyciskiem ✨ — bezczynna aplikacja nie zużywa wtedy CPU.

use std::{f32::consts::TAU, sync::Arc};

use egui::{
    Color32, ColorImage, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame,
    LayerId, Margin, Mesh, Painter, Pos2, Rect, Response, RichText, Sense, Shadow, Shape, Stroke,
    TextStyle, TextureHandle, TextureOptions, Ui, Vec2, pos2, text::LayoutJob, vec2,
};

// ── Paleta ─────────────────────────────────────────────────────────────

pub mod palette {
    use egui::Color32;

    /// Matowa czerń z nutą fioletu — baza całego interfejsu.
    pub const BG: Color32 = Color32::from_rgb(0x09, 0x08, 0x0e);
    /// Główny akcent: matowy, przygaszony fiolet (bez neonu).
    pub const VIOLET: Color32 = Color32::from_rgb(0x8b, 0x6c, 0xf0);
    pub const VIOLET_SOFT: Color32 = Color32::from_rgb(0xb9, 0xa6, 0xf7);
    pub const VIOLET_DEEP: Color32 = Color32::from_rgb(0x4a, 0x31, 0x96);
    /// Różowo-fioletowy — wzmianki i powiadomienia.
    pub const ORCHID: Color32 = Color32::from_rgb(0xd9, 0x7a, 0xf2);
    pub const TEXT: Color32 = Color32::from_rgb(0xf1, 0xef, 0xf7);
    pub const TEXT_DIM: Color32 = Color32::from_rgb(0xa6, 0xa1, 0xb8);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x6c, 0x67, 0x80);
    pub const OK: Color32 = Color32::from_rgb(0x6f, 0xd6, 0x9c);
    pub const WARN: Color32 = Color32::from_rgb(0xe8, 0xb8, 0x6b);
    pub const ERR: Color32 = Color32::from_rgb(0xf0, 0x78, 0x88);

    /// Biel o podanej nieprzezroczystości (kolory egui są premultiplikowane).
    pub const fn white(alpha: u8) -> Color32 {
        Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha)
    }

    pub const fn black(alpha: u8) -> Color32 {
        Color32::from_rgba_premultiplied(0, 0, 0, alpha)
    }

    /// Wypełnienie szkła i jego krawędź.
    pub const GLASS: Color32 = Color32::from_rgba_premultiplied(18, 15, 26, 150);
    pub const GLASS_EDGE: Color32 = white(26);
    pub const GLASS_HIGHLIGHT: Color32 = white(46);
}

use palette::*;

/// Kolor z przezroczystością (nieprzemnożony → przemnożony).
pub fn alpha(color: Color32, a: f32) -> Color32 {
    color.gamma_multiply(a.clamp(0.0, 1.0))
}

// ── Czcionki ───────────────────────────────────────────────────────────

const BRAND: &str = "brand";
const SEMI: &str = "semi";

/// Outfit Bold — nagłówki i elementy marki (geometryczna, jak logo).
pub fn brand(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BRAND.into()))
}

/// Inter SemiBold — nicki, zakładki, przyciski.
pub fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMI.into()))
}

/// JetBrains Mono — godziny, liczniki, etykiety techniczne.
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add("inter", include_bytes!("../../assets/fonts/Inter-Regular.ttf"));
    add("inter-semi", include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"));
    add("outfit", include_bytes!("../../assets/fonts/Outfit-Bold.ttf"));
    add("jetbrains", include_bytes!("../../assets/fonts/JetBrainsMono-Medium.ttf"));

    // Domyślne czcionki egui zostają na końcu list — zapewniają emoji (🛡 ⏱ 🗑…).
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "inter".into());
    let fallback = proportional.clone();
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "jetbrains".into());

    let with_fallback = |first: &str| std::iter::once(first.to_owned()).chain(fallback.iter().cloned()).collect();
    fonts.families.insert(FontFamily::Name(BRAND.into()), with_fallback("outfit"));
    fonts.families.insert(FontFamily::Name(SEMI.into()), with_fallback("inter-semi"));

    ctx.set_fonts(fonts);
}

// ── Styl widgetów ──────────────────────────────────────────────────────

pub fn install(ctx: &Context) {
    install_fonts(ctx);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, brand(22.0)),
            (TextStyle::Body, FontId::proportional(14.5)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Monospace, mono(12.5)),
        ]
        .into();

        style.animation_time = 0.18;
        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 6.0);
        s.button_padding = vec2(12.0, 6.0);
        s.interact_size.y = 30.0;
        s.window_margin = Margin::same(18);
        s.menu_margin = Margin::same(8);
        s.scroll = egui::style::ScrollStyle::floating();
        s.scroll.bar_width = 6.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(TEXT);
        v.panel_fill = Color32::TRANSPARENT;
        // Okna (picker, logowanie) leżą nad czatem — muszą być prawie nieprzezroczyste.
        v.window_fill = Color32::from_rgba_premultiplied(20, 17, 30, 245);
        v.window_stroke = Stroke::new(1.0_f32, GLASS_EDGE);
        v.window_corner_radius = CornerRadius::same(20);
        v.window_shadow = Shadow { offset: [0, 18], blur: 48, spread: 0, color: black(170) };
        v.menu_corner_radius = CornerRadius::same(14);
        v.popup_shadow = Shadow { offset: [0, 10], blur: 28, spread: 0, color: black(150) };
        v.extreme_bg_color = white(10);
        v.faint_bg_color = white(6);
        v.code_bg_color = white(10);
        v.selection.bg_fill = alpha(VIOLET, 0.55);
        v.selection.stroke = Stroke::new(1.0_f32, TEXT);
        v.hyperlink_color = VIOLET_SOFT;
        v.warn_fg_color = WARN;
        v.error_fg_color = ERR;
        v.text_cursor.stroke = Stroke::new(2.0_f32, VIOLET_SOFT);

        let radius = CornerRadius::same(10);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = Color32::TRANSPARENT;
        w.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, white(18));
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_DIM);
        w.noninteractive.corner_radius = radius;

        w.inactive.bg_fill = white(12);
        w.inactive.weak_bg_fill = white(12);
        w.inactive.bg_stroke = Stroke::new(1.0_f32, white(16));
        w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
        w.inactive.corner_radius = radius;

        w.hovered.bg_fill = white(22);
        w.hovered.weak_bg_fill = white(22);
        w.hovered.bg_stroke = Stroke::new(1.0_f32, alpha(VIOLET, 0.7));
        w.hovered.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);
        w.hovered.corner_radius = radius;
        w.hovered.expansion = 1.0;

        w.active.bg_fill = alpha(VIOLET, 0.45);
        w.active.weak_bg_fill = alpha(VIOLET, 0.45);
        w.active.bg_stroke = Stroke::new(1.0_f32, VIOLET);
        w.active.fg_stroke = Stroke::new(2.0_f32, Color32::WHITE);
        w.active.corner_radius = radius;

        w.open.bg_fill = white(18);
        w.open.weak_bg_fill = white(18);
        w.open.bg_stroke = Stroke::new(1.0_f32, alpha(VIOLET, 0.6));
        w.open.corner_radius = radius;
    });
}

// ── Szkło ──────────────────────────────────────────────────────────────

/// Ramka szklanego panelu. `outer` to odstęp od krawędzi okna/sąsiadów.
pub fn glass(radius: u8, outer: Margin, inner: Margin) -> Frame {
    Frame {
        fill: GLASS,
        stroke: Stroke::new(1.0_f32, GLASS_EDGE),
        corner_radius: CornerRadius::same(radius),
        inner_margin: inner,
        outer_margin: outer,
        shadow: Shadow { offset: [0, 14], blur: 36, spread: 0, color: black(110) },
    }
}

/// Rysuje zawartość panelu, a pod nią (ale nad wypełnieniem ramki) połysk
/// szkła: jasną linię przy górnej krawędzi i delikatny gradient w dół.
/// Kształt jest wstawiany w miejsce zarezerwowane PRZED treścią, więc
/// nie przykrywa tekstu.
pub fn with_sheen<R>(ui: &mut Ui, inner: Margin, radius: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let slot = ui.painter().add(Shape::Noop);
    let result = add(ui);
    let rect = ui.min_rect() + inner;
    ui.painter().set(slot, sheen(rect, radius));
    result
}

fn sheen(rect: Rect, radius: f32) -> Shape {
    let inset = radius * 0.7;
    let top = rect.top() + 0.5;
    let line = Shape::line_segment(
        [pos2(rect.left() + inset, top), pos2(rect.right() - inset, top)],
        Stroke::new(1.0_f32, GLASS_HIGHLIGHT),
    );
    let glow_rect = Rect::from_min_max(
        pos2(rect.left() + inset * 0.5, top + radius * 0.25),
        pos2(rect.right() - inset * 0.5, top + (rect.height() * 0.45).min(70.0)),
    );
    Shape::Vec(vec![vertical_gradient(glow_rect, white(9), Color32::TRANSPARENT), line])
}

fn vertical_gradient(rect: Rect, top: Color32, bottom: Color32) -> Shape {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    Shape::mesh(mesh)
}

/// Miękka poświata (gradient radialny) — podstawa zorzy i efektów glow.
pub fn radial_glow(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    const SEGMENTS: u32 = 72;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color);
    // Pierścień pośredni łagodzi spadek jasności (zamiast liniowego stożka).
    let rings = [(0.42, color.gamma_multiply(0.62)), (0.72, color.gamma_multiply(0.22)), (1.0, Color32::TRANSPARENT)];
    for (fraction, ring_color) in rings {
        for i in 0..SEGMENTS {
            let angle = i as f32 / SEGMENTS as f32 * TAU;
            mesh.colored_vertex(center + Vec2::angled(angle) * radius * fraction, ring_color);
        }
    }
    for i in 0..SEGMENTS {
        let next = (i + 1) % SEGMENTS;
        mesh.add_triangle(0, 1 + i, 1 + next);
        for ring in 0..2 {
            let a = 1 + ring * SEGMENTS;
            let b = a + SEGMENTS;
            mesh.add_triangle(a + i, a + next, b + i);
            mesh.add_triangle(a + next, b + next, b + i);
        }
    }
    painter.add(mesh);
}

// ── Tło: zorza + ziarno ────────────────────────────────────────────────

struct Blob {
    anchor: Vec2,
    radius: f32,
    color: Color32,
    speed: f32,
    phase: f32,
}

const BLOBS: [Blob; 4] = [
    Blob { anchor: vec2(0.12, 0.08), radius: 0.85, color: Color32::from_rgba_premultiplied(58, 40, 120, 120), speed: 0.045, phase: 0.0 },
    Blob { anchor: vec2(0.95, 0.92), radius: 0.80, color: Color32::from_rgba_premultiplied(44, 26, 92, 125), speed: 0.035, phase: 2.1 },
    Blob { anchor: vec2(0.85, 0.12), radius: 0.50, color: Color32::from_rgba_premultiplied(70, 30, 82, 70), speed: 0.055, phase: 4.2 },
    Blob { anchor: vec2(0.40, 0.62), radius: 0.45, color: Color32::from_rgba_premultiplied(30, 28, 44, 50), speed: 0.065, phase: 1.3 },
];

#[derive(Default)]
pub struct Backdrop {
    grain: Option<TextureHandle>,
}

impl Backdrop {
    /// Rysuje tło całego okna. `translucent` — pod oknem jest systemowe
    /// szkło (Mica), więc baza jest półprzezroczysta.
    pub fn paint(&mut self, ctx: &Context, animate: bool, translucent: bool) {
        let painter = ctx.layer_painter(LayerId::background());
        let rect = ctx.screen_rect();
        let base = if translucent { Color32::from_rgba_premultiplied(5, 4, 8, 170) } else { BG };
        painter.rect_filled(rect, 0.0, base);

        let time = if animate { ctx.input(|i| i.time) as f32 } else { 0.0 };
        let size = rect.width().max(rect.height());
        for blob in &BLOBS {
            let t = time * blob.speed * TAU + blob.phase;
            let drift = vec2(t.sin() * 0.09, (t * 0.83).cos() * 0.07);
            let center = rect.min + (blob.anchor + drift) * rect.size();
            let breathing = 1.0 + (t * 1.3).sin() * 0.06;
            radial_glow(&painter, center, size * blob.radius * breathing, blob.color);
        }

        let grain = self.grain.get_or_insert_with(|| ctx.load_texture("grain", grain_image(), TextureOptions::NEAREST_REPEAT));
        let mut mesh = Mesh::with_texture(grain.id());
        let uv = Rect::from_min_size(Pos2::ZERO, rect.size() / GRAIN_SIZE as f32);
        mesh.add_rect_with_uv(rect, uv, white(14));
        painter.add(mesh);
    }
}

const GRAIN_SIZE: usize = 160;

/// Ziarno: deterministyczny szum (xorshift), białe piksele o losowej alfie.
fn grain_image() -> ColorImage {
    let mut state: u32 = 0x9e37_79b9;
    let pixels: Vec<u8> = (0..GRAIN_SIZE * GRAIN_SIZE)
        .flat_map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let a = (state >> 24) as u8;
            [255, 255, 255, a]
        })
        .collect();
    ColorImage::from_rgba_unmultiplied([GRAIN_SIZE, GRAIN_SIZE], &pixels)
}

/// Systemowe szkło pod oknem (Windows 11 Mica). Zwraca `true`, jeśli się udało.
pub fn try_system_glass(window: &impl raw_window_handle::HasWindowHandle) -> bool {
    match window_vibrancy::apply_mica(window, Some(true)) {
        Ok(()) => true,
        Err(e) => {
            tracing::info!("systemowe szkło niedostępne: {e}");
            false
        }
    }
}

// ── Widgety z charakterem ──────────────────────────────────────────────

fn now(ui: &Ui) -> f32 {
    ui.input(|i| i.time) as f32
}

/// Główny przycisk: matowy fioletowy „klejnot” z połyskiem, poświatą przy
/// najechaniu i delikatnym wciśnięciem przy kliknięciu.
pub fn accent_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), semi(14.0), Color32::WHITE);
    let size = galley.size() + vec2(30.0, 14.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let hover = ui.ctx().animate_bool_with_time(response.id, enabled && response.hovered(), 0.16);
    let pressed = enabled && response.is_pointer_button_down_on();
    let rect = if pressed { rect.shrink(1.0) } else { rect };
    let radius = rect.height() / 2.0;
    let painter = ui.painter();

    if hover > 0.0 {
        let glow = Shadow { offset: [0, 4], blur: 22, spread: 0, color: alpha(VIOLET, 0.55 * hover) };
        painter.add(glow.as_shape(rect, radius));
    }
    let opacity = if enabled { 1.0 } else { 0.35 };
    let base = VIOLET_DEEP.lerp_to_gamma(VIOLET, 0.55 + 0.25 * hover);
    painter.rect_filled(rect, radius, alpha(base, opacity));
    // Jaśniejsza górna połowa = połysk szkła.
    let top = Rect::from_min_max(rect.min + vec2(2.0, 1.5), pos2(rect.max.x - 2.0, rect.center().y));
    painter.rect_filled(top, CornerRadius { nw: radius as u8, ne: radius as u8, sw: 0, se: 0 }, white((14.0 * opacity) as u8));
    painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, white((40.0 * opacity) as u8)), egui::StrokeKind::Inside);
    painter.galley(rect.center() - galley.size() / 2.0, galley, alpha(Color32::WHITE, opacity));
    response
}

/// Kropka „na żywo” z rozchodzącym się pierścieniem (jak na aurorafut).
pub fn live_dot(ui: &mut Ui, color: Color32, animate: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    let center = rect.center();
    if animate {
        let t = (now(ui) % 1.8) / 1.8;
        let eased = 1.0 - (1.0 - t).powi(3);
        ui.painter().circle_filled(center, 4.0 + 6.0 * eased, alpha(color, 0.45 * (1.0 - t)));
    }
    ui.painter().circle_filled(center, 4.0, color);
}

/// Słupki aktywności czatu — tańczą szybciej i wyżej, gdy czat przyspiesza.
pub fn activity_bars(ui: &mut Ui, messages_per_sec: f32, animate: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 16.0), Sense::hover());
    let energy = (messages_per_sec / 4.0).clamp(0.12, 1.0);
    let speed = 3.0 + messages_per_sec.min(12.0) * 0.8;
    let time = if animate { now(ui) } else { 0.0 };
    for i in 0..4 {
        let wave = (time * speed + i as f32 * 1.7).sin().abs();
        let height = (0.25 + 0.75 * wave * energy) * rect.height();
        let x = rect.left() + 1.0 + i as f32 * 5.5;
        let bar = Rect::from_min_max(pos2(x, rect.bottom() - height), pos2(x + 3.0, rect.bottom()));
        let color = VIOLET.lerp_to_gamma(VIOLET_SOFT, i as f32 / 3.0);
        ui.painter().rect_filled(bar, 1.5, color);
    }
    response
}

/// Tekst z płynącym gradientem (fiolet → biel → orchidea), znak po znaku.
pub fn flowing_text(ui: &mut Ui, text: &str, font: FontId, animate: bool) -> Response {
    let time = if animate { now(ui) } else { 0.0 };
    let stops = [VIOLET, VIOLET_SOFT, TEXT, ORCHID, VIOLET];
    let count = text.chars().count().max(1) as f32;
    let mut job = LayoutJob::default();
    for (i, ch) in text.chars().enumerate() {
        let phase = (i as f32 / count * 0.6 + time * 0.12).rem_euclid(1.0) * (stops.len() - 1) as f32;
        let index = phase.floor() as usize;
        let color = stops[index].lerp_to_gamma(stops[(index + 1).min(stops.len() - 1)], phase.fract());
        job.append(&ch.to_string(), 0.0, egui::TextFormat::simple(font.clone(), color));
    }
    ui.label(job)
}

/// Mała etykieta w stylu „tagu” (mono, wersaliki).
pub fn tag(text: &str, color: Color32) -> RichText {
    RichText::new(text.to_uppercase()).font(mono(10.0)).color(color)
}
