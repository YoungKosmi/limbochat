//! LiMBOCHAT — punkt wejścia: okno eframe/egui. Runtime Tokio z backendem czatu
//! tworzy i posiada `ChatApp` (patrz `app.rs`).

// Na Windows w buildzie release nie otwieraj okna konsoli obok aplikacji.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod auth;
mod backend;
mod demo;
mod emotes;
mod events;
mod helix;
mod irc;
mod parser;
mod seventv;
mod ui;

use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,twitch_irc=warn")),
        )
        .init();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("LiMBOCHAT")
        .with_app_id("limbochat")
        .with_inner_size([540.0, 840.0])
        .with_min_inner_size([360.0, 420.0])
        // Przezroczyste okno tylko przy włączonym szkle systemowym — system
        // musi o tym wiedzieć w chwili tworzenia okna.
        .with_transparent(app::system_glass_enabled());
    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => tracing::warn!("nie udało się wczytać ikony: {e}"),
    }

    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        "LiMBOCHAT",
        options,
        Box::new(|cc| Ok(Box::new(app::ChatApp::new(cc)?))),
    )
}
