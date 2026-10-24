//! Narzędzia moderatorskie w UI:
//! * szybkie przyciski „one-click” przed wiadomością,
//! * menu kontekstowe (prawy przycisk na nicku) z pełnym zestawem akcji.
//!
//! Funkcje nie wykonują akcji same — dopisują `Command` do listy, którą
//! `ChatApp` po klatce wysyła do backendu, a ten wykonuje je przez Helix.

use egui::{Button, RichText, Ui};

use crate::{
    events::{ChannelRef, Command},
    parser::ParsedMessage,
};

/// Presety timeoutu w menu kontekstowym: (etykieta, sekundy).
pub const TIMEOUT_PRESETS: &[(&str, u32)] = &[
    ("1 minuta", 60),
    ("10 minut", 600),
    ("1 godzina", 3_600),
    ("24 godziny", 86_400),
    ("7 dni", 604_800),
];

/// Długość timeoutu pod przyciskiem one-click.
pub const QUICK_TIMEOUT_SECS: u32 = 600;

fn channel(msg: &ParsedMessage) -> ChannelRef {
    ChannelRef { login: msg.channel_login.clone(), id: msg.channel_id.clone() }
}

fn delete(msg: &ParsedMessage) -> Command {
    Command::DeleteMessage { channel: channel(msg), message_id: msg.id.clone() }
}

fn timeout(msg: &ParsedMessage, seconds: u32) -> Command {
    Command::Timeout {
        channel: channel(msg),
        user_id: msg.sender_id.clone(),
        login: msg.sender_login.clone(),
        seconds,
        reason: None,
    }
}

fn ban(msg: &ParsedMessage) -> Command {
    Command::Ban {
        channel: channel(msg),
        user_id: msg.sender_id.clone(),
        login: msg.sender_login.clone(),
        reason: None,
    }
}

/// Małe przyciski przed wiadomością: 🗑 usuń, ⏱ timeout 10 min, 🚫 ban.
/// Ban wymaga przytrzymania Shift — to celowa ochrona przed misclickiem
/// przy szybko przewijającym się czacie.
pub fn quick_actions(ui: &mut Ui, msg: &ParsedMessage, actions: &mut Vec<Command>) {
    let small = |label: &str| Button::new(RichText::new(label).small()).frame(false);

    if ui
        .add_enabled(!msg.deleted, small("🗑"))
        .on_hover_text("Usuń wiadomość")
        .clicked()
    {
        actions.push(delete(msg));
    }

    if ui
        .add(small("⏱"))
        .on_hover_text(format!("Timeout {} min", QUICK_TIMEOUT_SECS / 60))
        .clicked()
    {
        actions.push(timeout(msg, QUICK_TIMEOUT_SECS));
    }

    let shift = ui.input(|i| i.modifiers.shift);
    if ui
        .add_enabled(shift, small("🚫"))
        .on_hover_text("Ban (przytrzymaj Shift, aby odblokować)")
        .on_disabled_hover_text("Ban — przytrzymaj Shift i kliknij")
        .clicked()
    {
        actions.push(ban(msg));
    }
}

/// Menu pod prawym przyciskiem myszy na nicku użytkownika.
pub fn user_context_menu(ui: &mut Ui, msg: &ParsedMessage, can_moderate: bool, actions: &mut Vec<Command>) {
    ui.set_min_width(180.0);
    ui.label(RichText::new(&msg.sender_name).strong());
    ui.label(RichText::new(format!("ID: {}", msg.sender_id)).weak().small());
    ui.separator();

    if can_moderate {
        moderation_items(ui, msg, actions);
    } else {
        ui.label(RichText::new("Zaloguj się jako moderator\ntego kanału, aby moderować.").weak().small());
    }

    ui.separator();
    utility_items(ui, msg);
}

fn moderation_items(ui: &mut Ui, msg: &ParsedMessage, actions: &mut Vec<Command>) {
    if ui.add_enabled(!msg.deleted, Button::new("🗑  Usuń wiadomość")).clicked() {
        actions.push(delete(msg));
        ui.close();
    }

    ui.menu_button("⏱  Timeout", |ui| {
        for &(label, secs) in TIMEOUT_PRESETS {
            if ui.button(label).clicked() {
                actions.push(timeout(msg, secs));
                ui.close();
            }
        }
    });

    if ui.button(RichText::new("🚫  Ban").color(ui.visuals().error_fg_color)).clicked() {
        actions.push(ban(msg));
        ui.close();
    }

    if ui.button("✅  Unban / zdejmij timeout").clicked() {
        actions.push(Command::Unban {
            channel: channel(msg),
            user_id: msg.sender_id.clone(),
            login: msg.sender_login.clone(),
        });
        ui.close();
    }
}

fn utility_items(ui: &mut Ui, msg: &ParsedMessage) {
    if ui.button("📋  Kopiuj wiadomość").clicked() {
        ui.ctx().copy_text(msg.raw_text.clone());
        ui.close();
    }
    if ui.button("📋  Kopiuj nick").clicked() {
        ui.ctx().copy_text(msg.sender_login.clone());
        ui.close();
    }
    if ui.button("🔗  Otwórz profil").clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(format!("https://www.twitch.tv/{}", msg.sender_login)));
        ui.close();
    }
}
