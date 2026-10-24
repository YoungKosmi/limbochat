//! Pasek zakładek kanałów.
//!
//! Smaczek: podświetlenie aktywnej zakładki to szklana „pigułka”, która
//! płynnie przesuwa się i rozciąga do nowej zakładki (animowana pozycja
//! i szerokość). Pigułka jest rysowana POD etykietami dzięki miejscu
//! zarezerwowanemu w liście kształtów przed narysowaniem zakładek.

use egui::{Color32, Id, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

use super::theme::{self, alpha, palette::*};

pub struct TabInfo<'a> {
    pub label: &'a str,
    pub unread: usize,
    pub mentioned: bool,
    pub connected: bool,
}

pub enum TabAction {
    Activate(usize),
    Close(usize),
}

const HEIGHT: f32 = 32.0;

pub fn tab_strip(ui: &mut Ui, tabs: &[TabInfo<'_>], active: usize, animate: bool) -> Option<TabAction> {
    let mut action = None;
    let indicator_slot = ui.painter().add(Shape::Noop);
    let origin_x = ui.min_rect().left();
    let mut active_rect: Option<Rect> = None;

    for (index, tab) in tabs.iter().enumerate() {
        let selected = index == active;
        let id = Id::new(("tab", tab.label));

        let text_color = if selected {
            TEXT
        } else if tab.mentioned {
            ORCHID
        } else if tab.unread > 0 {
            TEXT
        } else {
            TEXT_DIM
        };
        let galley = ui.painter().layout_no_wrap(format!("#{}", tab.label), theme::semi(13.5), text_color);

        let badge = (!selected && tab.unread > 0).then(|| {
            let text = if tab.unread > 99 { "99+".to_owned() } else { tab.unread.to_string() };
            ui.painter().layout_no_wrap(text, theme::mono(10.0), Color32::WHITE)
        });
        let badge_w = badge.as_ref().map_or(0.0, |g| (g.size().x + 10.0).max(18.0) + 6.0);
        let width = 22.0 + galley.size().x + badge_w + 32.0;

        let (rect, response) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::click());
        let close_rect = Rect::from_center_size(pos2(rect.right() - 13.0, rect.center().y), vec2(16.0, 16.0));
        let close = ui.interact(close_rect, id.with("close"), Sense::click());

        let hover = ui.ctx().animate_bool_with_time(id.with("hover"), response.hovered() || close.hovered(), 0.15);
        let painter = ui.painter();
        if !selected && hover > 0.0 {
            painter.rect_filled(rect, HEIGHT / 2.0, white((10.0 * hover) as u8));
        }

        // Kropka stanu: zielona = połączono, bursztynowa = łączenie.
        let dot = pos2(rect.left() + 13.0, rect.center().y);
        let dot_color = if tab.connected { OK } else { WARN };
        painter.circle_filled(dot, 3.0, alpha(dot_color, if selected { 1.0 } else { 0.6 }));

        let text_pos = pos2(rect.left() + 22.0, rect.center().y - galley.size().y / 2.0);
        let text_w = galley.size().x;
        painter.galley(text_pos, galley, text_color);

        if let Some(badge) = badge {
            let size = vec2((badge.size().x + 10.0).max(18.0), 16.0);
            let pill = Rect::from_min_size(pos2(text_pos.x + text_w + 6.0, rect.center().y - 8.0), size);
            painter.rect_filled(pill, 8.0, if tab.mentioned { ORCHID } else { alpha(VIOLET, 0.85) });
            painter.galley(pill.center() - badge.size() / 2.0, badge, Color32::WHITE);
        }

        // „×” pojawia się przy najechaniu (zawsze widoczny na aktywnej).
        let close_visibility = if selected { 0.7 } else { hover };
        if close_visibility > 0.01 {
            let color = if close.hovered() { TEXT } else { alpha(TEXT_DIM, close_visibility) };
            if close.hovered() {
                painter.circle_filled(close_rect.center(), 8.0, white(22));
            }
            let c = close_rect.center();
            let d = 3.5;
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], Stroke::new(1.4_f32, color));
            painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], Stroke::new(1.4_f32, color));
        }

        if close.clicked() || response.middle_clicked() {
            action = Some(TabAction::Close(index));
        } else if response.clicked() {
            action = Some(TabAction::Activate(index));
        }
        if selected {
            active_rect = Some(rect);
        }
    }

    if let Some(target) = active_rect {
        // Animujemy we współrzędnych treści, żeby przewijanie paska nie „ciągnęło” pigułki.
        let (x, w) = if animate {
            let ctx = ui.ctx();
            (
                ctx.animate_value_with_time(Id::new("tab_indicator_x"), target.left() - origin_x, 0.28),
                ctx.animate_value_with_time(Id::new("tab_indicator_w"), target.width(), 0.28),
            )
        } else {
            (target.left() - origin_x, target.width())
        };
        let rect = Rect::from_min_size(pos2(origin_x + x, target.top()), vec2(w, target.height()));
        let radius = HEIGHT / 2.0;
        let glow = egui::Shadow { offset: [0, 2], blur: 16, spread: 0, color: alpha(VIOLET, 0.35) };
        let underline = Rect::from_center_size(pos2(rect.center().x, rect.bottom() - 1.5), vec2(w * 0.4, 2.0));
        ui.painter().set(
            indicator_slot,
            Shape::Vec(vec![
                glow.as_shape(rect, radius).into(),
                Shape::rect_filled(rect, radius, alpha(VIOLET, 0.24)),
                Shape::rect_stroke(rect, radius, Stroke::new(1.0_f32, alpha(VIOLET_SOFT, 0.45)), egui::StrokeKind::Inside),
                Shape::rect_filled(underline, 1.0, VIOLET_SOFT),
            ]),
        );
    }

    action
}
