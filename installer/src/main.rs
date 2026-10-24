//! LiMBOCHAT-Setup — instalator w jednym pliku, w tym samym stylu co aplikacja.
//!
//! Tryby:
//! * bez argumentów — instalacja lub aktualizacja (z oknem),
//! * `--uninstall` — dezinstalacja (tak uruchamia go „Aplikacje i funkcje”),
//! * `--silent` — cicha instalacja bez okna (dla zaawansowanych).

// W buildzie release nie pokazuj okna konsoli obok instalatora.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ops;

// Wspólny motyw z aplikacją (paleta, szkło, przyciski, tło zorzy).
#[allow(dead_code)]
#[path = "../../src/ui/theme.rs"]
mod theme;

use std::{
    sync::mpsc::{self, Receiver},
    thread,
};

use egui::{Color32, Image, Margin, RichText, Sense, Stroke, vec2};
use ops::{InstallOptions, Paths, Progress, UninstallOptions};
use theme::{Backdrop, alpha, palette::*};

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let uninstall = args.iter().any(|a| a == "--uninstall");

    if args.iter().any(|a| a == "--silent") && !uninstall {
        return silent_install();
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(if uninstall { "Dezinstalator LiMBOCHAT" } else { "Instalator LiMBOCHAT" })
        .with_app_id("limbochat-setup")
        .with_inner_size([460.0, 680.0])
        .with_resizable(false)
        .with_maximize_button(false);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }

    eframe::run_native(
        "LiMBOCHAT Setup",
        eframe::NativeOptions { viewport, persist_window: false, ..Default::default() },
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(Setup::new(if uninstall { Mode::Uninstall } else { Mode::Install })))
        }),
    )
}

fn silent_install() -> eframe::Result {
    let Some(paths) = Paths::detect() else {
        eprintln!("Nie można ustalić folderu instalacji.");
        std::process::exit(1);
    };
    let failed = std::cell::Cell::new(false);
    ops::install(&paths, &InstallOptions { desktop_shortcut: true }, &|p| match p {
        Progress::Step(i) => println!("→ {}", ops::INSTALL_STEPS[i]),
        Progress::Warning(w) => eprintln!("! {w}"),
        Progress::Failed(e) => {
            eprintln!("✗ {e}");
            failed.set(true);
        }
        Progress::Done => println!("✓ Zainstalowano w {}", paths.dir.display()),
        Progress::Fraction(_) => {}
    });
    std::process::exit(if failed.get() { 1 } else { 0 });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Install,
    Uninstall,
}

enum Screen {
    Welcome,
    Working { step: usize, fraction: f32 },
    Done { at: f64 },
    Failed(String),
}

struct Setup {
    mode: Mode,
    paths: Option<Paths>,
    screen: Screen,
    desktop_shortcut: bool,
    launch_after: bool,
    remove_data: bool,
    warnings: Vec<String>,
    progress: Option<Receiver<Progress>>,
    backdrop: Backdrop,
    uninstalled: bool,
}

impl Setup {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            paths: Paths::detect(),
            screen: Screen::Welcome,
            desktop_shortcut: true,
            launch_after: true,
            remove_data: false,
            warnings: Vec::new(),
            progress: None,
            backdrop: Backdrop::default(),
            uninstalled: false,
        }
    }

    fn steps(&self) -> &'static [&'static str] {
        match self.mode {
            Mode::Install => ops::INSTALL_STEPS,
            Mode::Uninstall => ops::UNINSTALL_STEPS,
        }
    }

    /// Uruchamia instalację/dezinstalację w osobnym wątku, żeby okno
    /// pozostało płynne (rozpakowywanie ~20 MB trwa chwilę).
    fn start(&mut self, ctx: &egui::Context) {
        let Some(paths) = self.paths.clone() else {
            self.screen = Screen::Failed("Nie można ustalić folderu instalacji.".into());
            return;
        };
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let mode = self.mode;
        let desktop_shortcut = self.desktop_shortcut;
        let remove_data = self.remove_data;

        thread::spawn(move || {
            let report = |p: Progress| {
                let _ = tx.send(p);
                ctx.request_repaint();
            };
            match mode {
                Mode::Install => ops::install(&paths, &InstallOptions { desktop_shortcut }, &report),
                Mode::Uninstall => ops::uninstall(&paths, &UninstallOptions { remove_data }, &report),
            }
        });

        self.warnings.clear();
        self.progress = Some(rx);
        self.screen = Screen::Working { step: 0, fraction: 0.0 };
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.progress else { return };
        let mut finished = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                Progress::Step(step) => self.screen = Screen::Working { step, fraction: 0.0 },
                Progress::Fraction(f) => {
                    if let Screen::Working { fraction, .. } = &mut self.screen {
                        *fraction = f;
                    }
                }
                Progress::Warning(w) => self.warnings.push(w),
                Progress::Done => {
                    self.screen = Screen::Done { at: ctx.input(|i| i.time) };
                    self.uninstalled = self.mode == Mode::Uninstall;
                    finished = true;
                }
                Progress::Failed(e) => {
                    self.screen = Screen::Failed(e);
                    finished = true;
                }
            }
        }
        if finished {
            self.progress = None;
        }
    }

    fn close(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    // ── Ekrany ─────────────────────────────────────────────────────────

    fn header(&self, ui: &mut egui::Ui) {
        let time = ui.input(|i| i.time) as f32;
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            let width = 200.0;
            let (rect, _) = ui.allocate_exact_size(vec2(width, width * 550.0 / 900.0), Sense::hover());
            let rect = rect.translate(vec2(0.0, (time * 1.2).sin() * 4.0));
            let pulse = (time * 1.6).sin() * 0.5 + 0.5;
            theme::radial_glow(
                &ui.ctx().layer_painter(egui::LayerId::background()),
                rect.center(),
                width * 0.8,
                alpha(VIOLET, 0.2 + 0.1 * pulse),
            );
            Image::new(egui::include_image!("../../assets/logo.png")).paint_at(ui, rect);
            let title = match self.mode {
                Mode::Install => "Instalator",
                Mode::Uninstall => "Dezinstalator",
            };
            theme::flowing_text(ui, title, theme::brand(24.0), true);
            ui.label(theme::tag(&format!("LiMBOCHAT · wersja {}", ops::VERSION), TEXT_FAINT));
            ui.add_space(10.0);
        });
    }

    fn welcome_install(&mut self, ui: &mut egui::Ui) {
        let installed = self.paths.as_ref().is_some_and(Paths::is_installed);
        let mut start = false;
        let mut uninstall = false;

        ui.vertical_centered(|ui| {
            let intro = if installed {
                "LiMBOCHAT jest już zainstalowany. Instalacja zaktualizuje go do tej wersji — ustawienia i logowanie zostaną."
            } else {
                "Czat Twitcha z emotkami 7TV i narzędziami moderatora. Po instalacji zaloguj się własnym kontem Twitch."
            };
            ui.label(RichText::new(intro).color(TEXT_DIM));
        });
        ui.add_space(14.0);

        info_card(ui, |ui| {
            ui.label(theme::tag("Folder instalacji", TEXT_FAINT));
            let path = self.paths.as_ref().map_or("—".into(), |p| p.dir.display().to_string());
            ui.label(RichText::new(path).font(theme::mono(11.5)).color(TEXT));
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("Rozmiar: {:.1} MB · bez uprawnień administratora", ops::app_size_mb()))
                    .size(12.0)
                    .color(TEXT_FAINT),
            );
        });
        ui.add_space(10.0);
        ui.checkbox(&mut self.desktop_shortcut, "Skrót na pulpicie");
        ui.checkbox(&mut self.launch_after, "Uruchom LiMBOCHAT po instalacji");

        ui.add_space(18.0);
        ui.vertical_centered(|ui| {
            let label = if installed { "Aktualizuj" } else { "Zainstaluj" };
            if big_button(ui, label) {
                start = true;
            }
            if installed {
                ui.add_space(6.0);
                if ui.add(egui::Button::new(RichText::new("Odinstaluj").color(TEXT_DIM)).frame(false)).clicked() {
                    uninstall = true;
                }
            }
        });

        if uninstall {
            self.mode = Mode::Uninstall;
        } else if start {
            self.start(ui.ctx());
        }
    }

    fn welcome_uninstall(&mut self, ui: &mut egui::Ui) {
        let mut start = false;
        let mut cancel = false;
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Czy na pewno chcesz odinstalować LiMBOCHAT?").font(theme::semi(16.0)));
            ui.add_space(4.0);
            ui.label(RichText::new("Aplikacja, skróty i wpis w systemie zostaną usunięte.").color(TEXT_DIM));
        });
        ui.add_space(14.0);
        info_card(ui, |ui| {
            ui.checkbox(&mut self.remove_data, "Usuń też ustawienia i zapisane logowanie Twitch");
            ui.label(
                RichText::new("Zostaw odznaczone, jeśli planujesz zainstalować aplikację ponownie.")
                    .size(12.0)
                    .color(TEXT_FAINT),
            );
        });
        ui.add_space(18.0);
        ui.vertical_centered(|ui| {
            if big_button(ui, "Odinstaluj") {
                start = true;
            }
            ui.add_space(6.0);
            if ui.add(egui::Button::new(RichText::new("Anuluj").color(TEXT_DIM)).frame(false)).clicked() {
                cancel = true;
            }
        });
        if start {
            self.start(ui.ctx());
        }
        if cancel {
            Self::close(ui.ctx());
        }
    }

    fn working(&self, ui: &mut egui::Ui, current: usize, fraction: f32) {
        let steps = self.steps();
        let time = ui.input(|i| i.time) as f32;

        info_card(ui, |ui| {
            for (index, label) in steps.iter().enumerate() {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                    let c = rect.center();
                    let painter = ui.painter();
                    if index < current {
                        painter.circle_filled(c, 8.0, alpha(OK, 0.2));
                        check_mark(painter, c, 4.5, 1.0, OK);
                    } else if index == current {
                        // Obracający się łuk = trwa.
                        let start = time * 5.0;
                        let points: Vec<_> = (0..=16)
                            .map(|i| c + egui::Vec2::angled(start + i as f32 / 16.0 * 4.2) * 7.0)
                            .collect();
                        painter.add(egui::Shape::line(points, Stroke::new(2.0_f32, VIOLET_SOFT)));
                    } else {
                        painter.circle_stroke(c, 6.0, Stroke::new(1.2_f32, TEXT_FAINT));
                    }
                    let color = if index == current { TEXT } else if index < current { TEXT_DIM } else { TEXT_FAINT };
                    ui.label(RichText::new(*label).color(color));
                });
            }
        });

        ui.add_space(16.0);
        let overall = ((current as f32 + fraction) / steps.len() as f32).clamp(0.0, 1.0);
        progress_bar(ui, overall, time);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(format!("{:.0}%", overall * 100.0)).font(theme::mono(12.0)).color(TEXT_DIM));
        });
        ui.ctx().request_repaint();
    }

    fn done(&mut self, ui: &mut egui::Ui, at: f64) {
        let elapsed = (ui.input(|i| i.time) - at) as f32;
        let mut finish = false;

        ui.vertical_centered(|ui| {
            // Animowany znaczek: okrąg się rysuje, potem „ptaszek”.
            let (rect, _) = ui.allocate_exact_size(vec2(84.0, 84.0), Sense::hover());
            let c = rect.center();
            let ring = (elapsed / 0.5).clamp(0.0, 1.0);
            let tick = ((elapsed - 0.35) / 0.35).clamp(0.0, 1.0);
            let painter = ui.painter();
            let glow = egui::Shadow { offset: [0, 0], blur: 30, spread: 0, color: alpha(OK, 0.35 * ring) };
            painter.add(glow.as_shape(egui::Rect::from_center_size(c, vec2(64.0, 64.0)), 32.0));
            let arc: Vec<_> = (0..=48)
                .map(|i| c + egui::Vec2::angled(-std::f32::consts::FRAC_PI_2 + i as f32 / 48.0 * ring * std::f32::consts::TAU) * 32.0)
                .collect();
            painter.add(egui::Shape::line(arc, Stroke::new(3.0_f32, OK)));
            check_mark(painter, c, 14.0, tick, OK);
            if elapsed < 1.0 {
                ui.ctx().request_repaint();
            }

            ui.add_space(10.0);
            let (title, text) = match self.mode {
                Mode::Install => (
                    "Gotowe!",
                    "LiMBOCHAT jest zainstalowany. Znajdziesz go w menu Start. Po uruchomieniu kliknij „Zaloguj przez Twitch” w prawym górnym rogu i zaloguj się swoim kontem.",
                ),
                Mode::Uninstall => ("Odinstalowano", "LiMBOCHAT został usunięty z komputera. Do zobaczenia!"),
            };
            ui.label(RichText::new(title).font(theme::brand(26.0)));
            ui.add_space(4.0);
            ui.label(RichText::new(text).color(TEXT_DIM));
        });

        if !self.warnings.is_empty() {
            ui.add_space(10.0);
            info_card(ui, |ui| {
                for warning in &self.warnings {
                    ui.label(RichText::new(format!("▲ {warning}")).size(12.0).color(WARN));
                }
            });
        }

        ui.add_space(18.0);
        ui.vertical_centered(|ui| {
            let label = match self.mode {
                Mode::Install if self.launch_after => "Uruchom LiMBOCHAT",
                _ => "Zamknij",
            };
            if big_button(ui, label) {
                finish = true;
            }
        });

        if finish {
            if self.mode == Mode::Install && self.launch_after {
                if let Some(paths) = &self.paths {
                    if let Err(e) = ops::launch(paths) {
                        self.screen = Screen::Failed(e);
                        return;
                    }
                }
            }
            Self::close(ui.ctx());
        }
    }

    fn failed(&mut self, ui: &mut egui::Ui, error: &str) {
        let mut retry = false;
        let mut close = false;
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Coś poszło nie tak").font(theme::brand(22.0)).color(ERR));
            ui.add_space(8.0);
        });
        info_card(ui, |ui| {
            ui.label(RichText::new(error).color(TEXT));
        });
        ui.add_space(18.0);
        ui.vertical_centered(|ui| {
            if big_button(ui, "Spróbuj ponownie") {
                retry = true;
            }
            ui.add_space(6.0);
            if ui.add(egui::Button::new(RichText::new("Zamknij").color(TEXT_DIM)).frame(false)).clicked() {
                close = true;
            }
        });
        if retry {
            self.start(ui.ctx());
        }
        if close {
            Self::close(ui.ctx());
        }
    }
}

impl eframe::App for Setup {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.backdrop.paint(ctx, true, false);

        let inner = Margin::same(22);
        egui::CentralPanel::default()
            .frame(theme::glass(24, Margin::same(14), inner))
            .show(ctx, |ui| {
                theme::with_sheen(ui, inner, 24.0, |ui| {
                    self.header(ui);
                    match std::mem::replace(&mut self.screen, Screen::Welcome) {
                        Screen::Welcome => {
                            self.screen = Screen::Welcome;
                            match self.mode {
                                Mode::Install => self.welcome_install(ui),
                                Mode::Uninstall => self.welcome_uninstall(ui),
                            }
                        }
                        Screen::Working { step, fraction } => {
                            self.screen = Screen::Working { step, fraction };
                            self.working(ui, step, fraction);
                        }
                        Screen::Done { at } => {
                            self.screen = Screen::Done { at };
                            self.done(ui, at);
                        }
                        Screen::Failed(error) => {
                            self.screen = Screen::Failed(error.clone());
                            self.failed(ui, &error);
                        }
                    }
                });
            });

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        // Po dezinstalacji usuń folder razem z tym programem (po jego zamknięciu).
        if self.uninstalled {
            if let Some(paths) = &self.paths {
                ops::schedule_self_removal(&paths.dir);
            }
        }
    }
}

// ── Drobne widgety ─────────────────────────────────────────────────────

fn info_card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame {
        fill: white(8),
        stroke: Stroke::new(1.0_f32, white(16)),
        corner_radius: egui::CornerRadius::same(14),
        inner_margin: Margin::same(14),
        ..Default::default()
    }
    .show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn big_button(ui: &mut egui::Ui, label: &str) -> bool {
    theme::accent_button(ui, label, true).clicked()
}

fn check_mark(painter: &egui::Painter, center: egui::Pos2, size: f32, progress: f32, color: Color32) {
    if progress <= 0.0 {
        return;
    }
    let a = center + vec2(-size * 0.9, 0.0);
    let b = center + vec2(-size * 0.25, size * 0.65);
    let c = center + vec2(size, -size * 0.7);
    let stroke = Stroke::new((size / 5.0).max(1.8), color);
    let first = (progress / 0.4).min(1.0);
    painter.line_segment([a, a + (b - a) * first], stroke);
    if progress > 0.4 {
        let second = ((progress - 0.4) / 0.6).min(1.0);
        painter.line_segment([b, b + (c - b) * second], stroke);
    }
}

/// Pasek postępu z przesuwającym się połyskiem (jak „shimmer” na aurorafut).
fn progress_bar(ui: &mut egui::Ui, fraction: f32, time: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, white(14));
    let mut filled = rect;
    filled.set_width((rect.width() * fraction).max(8.0));
    painter.rect_filled(filled, 4.0, VIOLET);
    let shimmer_x = filled.left() + (time * 0.8).fract() * (filled.width() + 60.0) - 30.0;
    let shimmer = egui::Rect::from_center_size(egui::pos2(shimmer_x, filled.center().y), vec2(36.0, filled.height()))
        .intersect(filled);
    if shimmer.width() > 0.0 {
        painter.rect_filled(shimmer, 4.0, white(50));
    }
    let glow = egui::Shadow { offset: [0, 0], blur: 14, spread: 0, color: alpha(VIOLET, 0.5) };
    painter.add(glow.as_shape(filled, 4.0));
}
