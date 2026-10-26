//! Główny stan LiMBOCHAT i pętla `eframe::App::update`.
//!
//! Każdy otwarty kanał to zakładka (`ChannelTab`) z własną historią czatu,
//! polem wiadomości, uprawnieniami i odznakami. Backend czyta wszystkie
//! kanały naraz, a UI kieruje zdarzenia do właściwej zakładki po loginie.
//!
//! Układ okna (wszystko to szklane panele nad animowaną zorzą):
//! ┌ nagłówek: logo · konto · przełączniki ┐
//! ├ zakładki kanałów                       ┤
//! │ czat                                   │
//! ├ pole wiadomości (szklana pigułka)      ┤
//! └ pasek statusu                          ┘

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{Align, Align2, Color32, Image, Key, Layout, Margin, RichText, Sense, Stroke, vec2};
use tokio::{
    runtime::Runtime,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};

use crate::{
    backend::{self, BackendConfig},
    emotes::{Emote, EmoteMap, EmoteScale, EmoteStore},
    events::{AuthState, ChannelRef, Command, UiEvent, UiSender},
    helix::BadgeMap,
    ui::{
        chat_view::{ChatView, RenderOptions, SystemLevel},
        composer::Composer,
        tabs::{self, TabAction, TabInfo},
        theme::{self, Backdrop, alpha, palette::*},
    },
};

const STORAGE_CHANNELS: &str = "channels";
const STORAGE_ACTIVE: &str = "active_tab";
const STORAGE_MOD_MODE: &str = "mod_mode";
const STORAGE_TIMESTAMPS: &str = "timestamps";
const STORAGE_ANIMATIONS: &str = "animations";
/// Limit zdarzeń na klatkę — przy zalewie wiadomości (raid, spam) klatka
/// pozostaje krótka, a reszta zostaje obsłużona w następnej.
const MAX_EVENTS_PER_FRAME: usize = 1_000;
const MAX_ROWS_PER_TAB: usize = 5_000;
/// Animacje tła: ~30 klatek/s wystarcza dla wolno płynącej zorzy.
const ANIMATION_FRAME: Duration = Duration::from_millis(33);
const GAP: i8 = 10;

/// Plik-flaga włączający systemowe szkło. Musi być odczytany PRZED
/// utworzeniem okna (przezroczystość ustawia się przy starcie), dlatego
/// nie trzymamy go w ustawieniach eframe.
pub fn system_glass_flag() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "LiMBOCHAT").map(|d| d.config_dir().join("system-glass"))
}

pub fn system_glass_enabled() -> bool {
    system_glass_flag().is_some_and(|p| p.exists())
}

fn set_system_glass(enabled: bool) -> std::io::Result<()> {
    let Some(path) = system_glass_flag() else { return Ok(()) };
    if enabled {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, b"1")
    } else if path.exists() {
        std::fs::remove_file(path)
    } else {
        Ok(())
    }
}

struct ChannelTab {
    login: String,
    channel_id: Option<String>,
    chat: ChatView,
    composer: Composer,
    can_moderate: bool,
    channel_badges: Arc<BadgeMap>,
    /// Globalne + kanałowe, przeliczane tylko przy zmianie którejś z nich.
    merged_badges: BadgeMap,
    /// Lista do pickera/podpowiedzi; `None` = do przeliczenia.
    emote_list: Option<Vec<Arc<Emote>>>,
    channel_emote_count: usize,
    unread: usize,
    mentioned: bool,
}

impl ChannelTab {
    fn new(login: String) -> Self {
        Self {
            login,
            channel_id: None,
            chat: ChatView::new(MAX_ROWS_PER_TAB),
            composer: Composer::default(),
            can_moderate: false,
            channel_badges: Arc::default(),
            merged_badges: BadgeMap::new(),
            emote_list: None,
            channel_emote_count: 0,
            unread: 0,
            mentioned: false,
        }
    }

    fn channel_ref(&self) -> Option<ChannelRef> {
        self.channel_id.as_ref().map(|id| ChannelRef { login: self.login.clone(), id: id.clone() })
    }

    fn rebuild_badges(&mut self, global: &BadgeMap) {
        let mut merged = global.clone();
        merged.extend(self.channel_badges.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.merged_badges = merged;
    }
}

pub struct ChatApp {
    /// `Option`, żeby w `Drop` móc go zabrać i zamknąć bez czekania.
    runtime: Option<Runtime>,
    events: UnboundedReceiver<UiEvent>,
    commands: UnboundedSender<Command>,
    emotes: Arc<EmoteStore>,
    emote_scale: EmoteScale,
    /// W trybie demo: lokalne emotki zamiast 7TV.
    demo_emotes: Option<EmoteMap>,

    tabs: Vec<ChannelTab>,
    active: usize,
    adding_tab: bool,
    new_tab_input: String,
    /// Komunikaty bez zakładki (np. przed otwarciem pierwszego kanału).
    lobby_log: VecDeque<(String, SystemLevel)>,

    mod_mode: bool,
    show_timestamps: bool,
    animations: bool,
    backdrop: Backdrop,
    /// System rozmywa pulpit pod oknem (Mica) — tło rysujemy półprzezroczyste.
    system_glass_active: bool,
    system_glass_wanted: bool,
    global_emote_count: usize,

    auth: AuthState,
    avatar_url: Option<String>,
    global_badges: Arc<BadgeMap>,
}

impl ChatApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        theme::install(&cc.egui_ctx);
        // Loadery obrazów egui: pobieranie po HTTP, cache, dekodowanie
        // PNG/GIF/WebP (łącznie z animacjami) w wątkach w tle.
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let system_glass_wanted = system_glass_enabled();
        let system_glass_active = system_glass_wanted && theme::try_system_glass(cc);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("limbochat-backend")
            .enable_all()
            .build()?;

        let (ui_tx, events) = mpsc::unbounded_channel();
        let ctx = cc.egui_ctx.clone();
        let ui_sender = UiSender::new(ui_tx, Arc::new(move || ctx.request_repaint()));

        let config = BackendConfig::default();
        let emote_scale = config.emote_scale;
        let backend::Backend { commands, emotes, .. } = backend::spawn(runtime.handle(), ui_sender, config);

        let storage = cc.storage;
        let get_bool = |key: &str, default: bool| storage.and_then(|s| eframe::get_value(s, key)).unwrap_or(default);
        let channels: Vec<String> = storage.and_then(|s| eframe::get_value(s, STORAGE_CHANNELS)).unwrap_or_default();
        let active: usize = storage.and_then(|s| eframe::get_value(s, STORAGE_ACTIVE)).unwrap_or(0);
        // Migracja z wersji jednokanałowej.
        let legacy = storage
            .and_then(|s| eframe::get_value::<String>(s, "last_channel"))
            .filter(|c| !c.trim().is_empty());

        let mut app = Self {
            runtime: Some(runtime),
            events,
            commands,
            emotes,
            emote_scale,
            demo_emotes: None,
            tabs: Vec::new(),
            active: 0,
            adding_tab: false,
            new_tab_input: String::new(),
            lobby_log: VecDeque::new(),
            mod_mode: get_bool(STORAGE_MOD_MODE, true),
            show_timestamps: get_bool(STORAGE_TIMESTAMPS, true),
            animations: get_bool(STORAGE_ANIMATIONS, true),
            backdrop: Backdrop::default(),
            system_glass_active,
            system_glass_wanted,
            global_emote_count: 0,
            auth: AuthState::Restoring,
            avatar_url: None,
            global_badges: Arc::default(),
        };

        if crate::demo::enabled() {
            app.setup_demo(&cc.egui_ctx);
        } else {
            for channel in channels.into_iter().chain(legacy) {
                app.open_tab(&channel, false);
            }
            app.active = active.min(app.tabs.len().saturating_sub(1));
        }
        Ok(app)
    }

    fn setup_demo(&mut self, ctx: &egui::Context) {
        let map = crate::demo::emotes(ctx);
        let mut tab = ChannelTab::new("demo".into());
        tab.channel_id = Some("0".into());
        tab.can_moderate = true;
        tab.emote_list = Some(map.sorted_for_picker());
        crate::demo::inject(&mut tab.chat, &map);
        self.tabs.push(tab);
        for (name, unread, mentioned) in [("drugi_kanal", 12, true), ("trzeci", 3, false)] {
            let mut tab = ChannelTab::new(name.into());
            tab.channel_id = Some("1".into());
            tab.unread = unread;
            tab.mentioned = mentioned;
            self.tabs.push(tab);
        }
        self.demo_emotes = Some(map);
        self.auth = AuthState::LoggedIn { login: "ty".into(), user_id: "0".into() };
    }

    fn is_demo(&self) -> bool {
        self.demo_emotes.is_some()
    }

    fn logged_in(&self) -> bool {
        matches!(self.auth, AuthState::LoggedIn { .. })
    }

    fn my_login(&self) -> Option<String> {
        match &self.auth {
            AuthState::LoggedIn { login, .. } => Some(login.to_lowercase()),
            _ => None,
        }
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            tracing::error!("backend nie działa — komenda odrzucona");
        }
    }

    // ── Zakładki ───────────────────────────────────────────────────────

    /// Otwiera kanał (albo przełącza na już otwarty). Zwraca `false` dla złej nazwy.
    fn open_tab(&mut self, name: &str, activate: bool) -> bool {
        let login = name.trim().trim_start_matches('#').to_lowercase();
        let valid = (1..=25).contains(&login.len()) && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return false;
        }

        if let Some(index) = self.tabs.iter().position(|t| t.login == login) {
            if activate {
                self.activate(index);
            }
            return true;
        }

        let mut tab = ChannelTab::new(login.clone());
        tab.rebuild_badges(&self.global_badges);
        self.tabs.push(tab);
        self.send(Command::Join(login));
        if activate {
            self.activate(self.tabs.len() - 1);
        }
        true
    }

    fn close_tab(&mut self, index: usize) {
        let tab = self.tabs.remove(index);
        self.send(Command::Part(tab.login));
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    fn activate(&mut self, index: usize) {
        self.active = index;
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.unread = 0;
            tab.mentioned = false;
        }
    }

    fn tab_mut(&mut self, login: &str) -> Option<&mut ChannelTab> {
        self.tabs.iter_mut().find(|t| t.login == login)
    }

    /// Dodaje komunikat systemowy do zakładki kanału, aktywnej albo do lobby.
    fn system(&mut self, channel: Option<String>, text: String, level: SystemLevel) {
        let active = self.active;
        let tab = match channel {
            Some(login) => self.tabs.iter_mut().find(|t| t.login == login),
            None => self.tabs.get_mut(active),
        };
        match tab {
            Some(tab) => tab.chat.push_system(text, level),
            None => {
                self.lobby_log.push_back((text, level));
                if self.lobby_log.len() > 6 {
                    self.lobby_log.pop_front();
                }
            }
        }
    }

    // ── Zdarzenia z backendu ───────────────────────────────────────────

    fn handle_events(&mut self, ctx: &egui::Context) {
        for _ in 0..MAX_EVENTS_PER_FRAME {
            let Ok(event) = self.events.try_recv() else { return };
            self.handle_event(event);
        }
        // Zostały zdarzenia w kolejce — obsłużymy je w następnej klatce.
        ctx.request_repaint();
    }

    fn handle_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Message(msg) => {
                let my_login = self.my_login();
                let active_login = self.tabs.get(self.active).map(|t| t.login.clone());
                let channel = msg.channel_login.clone();
                if let Some(tab) = self.tab_mut(&channel) {
                    if active_login.as_deref() != Some(channel.as_str()) {
                        tab.unread += 1;
                        if let Some(me) = my_login {
                            if msg.raw_text.to_lowercase().contains(&me) && msg.sender_login != me {
                                tab.mentioned = true;
                            }
                        }
                    }
                    tab.chat.push_message(msg);
                }
            }
            UiEvent::MessageDeleted { channel, message_id } => {
                if let Some(tab) = self.tab_mut(&channel) {
                    tab.chat.mark_message_deleted(&message_id);
                }
            }
            UiEvent::UserCleared { channel, user_id, login, timeout_secs } => {
                if let Some(tab) = self.tab_mut(&channel) {
                    tab.chat.mark_user_cleared(&user_id);
                    let text = match timeout_secs {
                        Some(s) => format!("{login} otrzymał timeout na {}", format_duration(s)),
                        None => format!("{login} został zbanowany"),
                    };
                    tab.chat.push_system(text, SystemLevel::Info);
                }
            }
            UiEvent::ChatCleared { channel } => {
                if let Some(tab) = self.tab_mut(&channel) {
                    tab.chat.mark_all_deleted();
                    tab.chat.push_system("Czat został wyczyszczony przez moderatora", SystemLevel::Info);
                }
            }
            UiEvent::Connected { channel, channel_id } => {
                if let Some(tab) = self.tab_mut(&channel) {
                    tab.channel_id = Some(channel_id);
                    tab.chat.push_system(format!("Połączono z #{channel}"), SystemLevel::Info);
                }
            }
            UiEvent::EmotesLoaded { channel, count } => {
                let store = &self.emotes;
                let scale = self.emote_scale;
                match channel {
                    None => {
                        self.global_emote_count = count;
                        // Nowe emotki globalne dotyczą wszystkich zakładek.
                        for tab in &mut self.tabs {
                            let map = store.map_for(tab.channel_id.as_deref().unwrap_or_default());
                            tab.chat.retokenize(&map, scale);
                            tab.emote_list = None;
                        }
                    }
                    Some(login) => {
                        if let Some(tab) = self.tabs.iter_mut().find(|t| t.login == login) {
                            tab.channel_emote_count = count;
                            let map = store.map_for(tab.channel_id.as_deref().unwrap_or_default());
                            // Wiadomości sprzed odpowiedzi 7TV dostają emotki teraz.
                            tab.chat.retokenize(&map, scale);
                            tab.emote_list = None;
                        }
                    }
                }
            }
            // W demo sesja jest udawana — prawdziwy backend nie może jej nadpisać.
            UiEvent::Auth(_) if self.is_demo() => {}
            UiEvent::Auth(state) => {
                if let AuthState::LoggedIn { login, .. } = &state {
                    if matches!(self.auth, AuthState::Pending { .. }) {
                        self.system(None, format!("Zalogowano jako {login}"), SystemLevel::Info);
                    }
                }
                if matches!(state, AuthState::LoggedOut) {
                    self.avatar_url = None;
                    for tab in &mut self.tabs {
                        tab.can_moderate = false;
                    }
                }
                self.auth = state;
            }
            UiEvent::Avatar { url } => self.avatar_url = Some(url),
            UiEvent::ModStatus { channel, can_moderate } => {
                if let Some(tab) = self.tab_mut(&channel) {
                    tab.can_moderate = can_moderate;
                    if !can_moderate {
                        tab.chat.push_system(
                            "Nie masz uprawnień moderatora na tym kanale — możesz pisać, ale nie moderować.",
                            SystemLevel::Info,
                        );
                    }
                }
            }
            UiEvent::Badges { channel, map } => match channel {
                None => {
                    self.global_badges = map;
                    let global = self.global_badges.clone();
                    for tab in &mut self.tabs {
                        tab.rebuild_badges(&global);
                    }
                }
                Some(login) => {
                    let global = self.global_badges.clone();
                    if let Some(tab) = self.tab_mut(&login) {
                        tab.channel_badges = map;
                        tab.rebuild_badges(&global);
                    }
                }
            },
            UiEvent::Notice { channel, text } => self.system(channel, text, SystemLevel::Info),
            // Aplikacja PC: alert jako linia systemowa + ewentualna wiadomość.
            UiEvent::Alert(alert) => {
                let channel = alert.channel_login.clone();
                self.system(Some(channel), format!("✨ {}", alert_line(&alert)), SystemLevel::Info);
                if let Some(msg) = alert.message {
                    self.handle_event(UiEvent::Message(Box::new(msg)));
                }
            }
            UiEvent::Error { channel, text } => self.system(channel, text, SystemLevel::Error),
        }
    }

    // ── Nagłówek ───────────────────────────────────────────────────────

    fn header(&mut self, ctx: &egui::Context) {
        let inner = Margin::symmetric(14, 10);
        let frame = theme::glass(20, Margin { left: GAP, right: GAP, top: GAP, bottom: 4 }, inner);
        egui::TopBottomPanel::top("header").frame(frame).show_separator_line(false).show(ctx, |ui| {
            theme::with_sheen(ui, inner, 20.0, |ui| {
                ui.horizontal(|ui| {
                    self.brand(ui);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.account_widget(ui);
                        ui.add_space(4.0);
                        self.settings_menu(ui);
                        ui.toggle_value(&mut self.mod_mode, RichText::new("🛡").size(15.0))
                            .on_hover_text("Szybkie przyciski moderatorskie");
                    });
                });
                ui.add_space(6.0);
                self.tab_row(ui);
            });
        });
    }

    fn brand(&mut self, ui: &mut egui::Ui) {
        let height = 40.0;
        let logo = Image::new(egui::include_image!("../assets/logo.png")).fit_to_exact_size(vec2(height * 900.0 / 550.0, height));
        let response = ui.add(logo);
        // Oddychająca poświata pod logo.
        let pulse = if self.animations { (ui.input(|i| i.time) as f32 * 1.4).sin() * 0.5 + 0.5 } else { 0.5 };
        let glow_painter = ui.ctx().layer_painter(egui::LayerId::background());
        theme::radial_glow(&glow_painter, response.rect.center(), 70.0 + 10.0 * pulse, alpha(VIOLET, 0.16 + 0.10 * pulse));
        theme::flowing_text(ui, "CHAT", theme::brand(21.0), self.animations);
    }

    fn settings_menu(&mut self, ui: &mut egui::Ui) {
        let mut glass_changed = None;
        ui.menu_button(RichText::new("⚙").size(16.0), |ui| {
            ui.set_min_width(250.0);
            ui.label(theme::tag("Wygląd", TEXT_FAINT));
            ui.checkbox(&mut self.animations, "✨ Animacje (tło, przejścia)");
            ui.checkbox(&mut self.show_timestamps, "🕑 Godziny wiadomości");
            ui.checkbox(&mut self.mod_mode, "🛡 Szybkie przyciski moderacji");
            ui.separator();
            let mut wanted = self.system_glass_wanted;
            if ui.checkbox(&mut wanted, "🪟 Szkło systemowe (Windows 11)").changed() {
                glass_changed = Some(wanted);
            }
            let note = if self.system_glass_wanted && !self.system_glass_active {
                "Zadziała po ponownym uruchomieniu (lub system go nie obsługuje)."
            } else {
                "Rozmyty pulpit pod oknem. Wymaga ponownego uruchomienia."
            };
            ui.label(RichText::new(note).small().color(TEXT_FAINT));
        });
        if let Some(wanted) = glass_changed {
            match set_system_glass(wanted) {
                Ok(()) => self.system_glass_wanted = wanted,
                Err(e) => self.system(None, format!("Nie udało się zapisać ustawienia: {e}"), SystemLevel::Error),
            }
        }
    }

    fn account_widget(&mut self, ui: &mut egui::Ui) {
        let mut command = None;
        match &self.auth {
            AuthState::Restoring => {
                ui.spinner();
            }
            AuthState::LoggedOut => {
                if theme::accent_button(ui, "Zaloguj przez Twitch", true).clicked() {
                    command = Some(Command::Login);
                }
            }
            AuthState::Pending { .. } => {
                ui.spinner();
                ui.label(RichText::new("Logowanie…").color(TEXT_DIM));
            }
            AuthState::LoggedIn { login, user_id } => {
                let (login, user_id) = (login.clone(), user_id.clone());
                let avatar_url = self.avatar_url.clone();
                let animate = self.animations;
                let response = avatar_chip(ui, &login, avatar_url.as_deref(), animate);
                egui::Popup::menu(&response).show(|ui| {
                    ui.set_min_width(180.0);
                    ui.label(RichText::new(&login).font(theme::semi(15.0)));
                    ui.label(RichText::new(format!("ID: {user_id}")).font(theme::mono(10.5)).color(TEXT_FAINT));
                    ui.separator();
                    if ui.button("Wyloguj").clicked() {
                        command = Some(Command::Logout);
                        ui.close();
                    }
                });
            }
        }
        if let Some(cmd) = command {
            self.send(cmd);
        }
    }

    fn tab_row(&mut self, ui: &mut egui::Ui) {
        let infos: Vec<TabInfo<'_>> = self
            .tabs
            .iter()
            .map(|t| TabInfo {
                label: &t.login,
                unread: t.unread,
                mentioned: t.mentioned,
                connected: t.channel_id.is_some(),
            })
            .collect();

        let mut action = None;
        let mut add: Option<String> = None;
        let mut start_adding = false;
        let mut cancel_adding = false;

        egui::ScrollArea::horizontal().auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                action = tabs::tab_strip(ui, &infos, self.active, self.animations);
                ui.add_space(4.0);

                if self.adding_tab {
                    let input = ui.add(
                        egui::TextEdit::singleline(&mut self.new_tab_input)
                            .hint_text("nazwa kanału ↵")
                            .desired_width(150.0)
                            .margin(vec2(10.0, 6.0)),
                    );
                    input.request_focus();
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        add = Some(std::mem::take(&mut self.new_tab_input));
                    }
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        cancel_adding = true;
                    }
                } else if add_tab_button(ui).on_hover_text("Dołącz do kanału").clicked() {
                    start_adding = true;
                }
            });
        });
        drop(infos);

        if start_adding {
            self.adding_tab = true;
        }
        if cancel_adding {
            self.adding_tab = false;
            self.new_tab_input.clear();
        }
        match action {
            Some(TabAction::Close(index)) => self.close_tab(index),
            Some(TabAction::Activate(index)) => self.activate(index),
            None => {}
        }
        if let Some(name) = add {
            self.adding_tab = false;
            if !name.trim().is_empty() && !self.open_tab(&name, true) {
                self.system(None, format!("„{}” to niepoprawna nazwa kanału Twitch", name.trim()), SystemLevel::Error);
            }
        }
    }

    // ── Dół okna ───────────────────────────────────────────────────────

    fn composer_bar(&mut self, ctx: &egui::Context) {
        let logged_in = self.logged_in();
        let demo = self.is_demo();
        let animate = self.animations;
        let store = self.emotes.clone();
        let Some(tab) = self.tabs.get_mut(self.active) else { return };

        let enabled = logged_in && tab.channel_id.is_some();
        let hint = if !logged_in {
            "Zaloguj się, aby pisać na czacie".to_owned()
        } else if tab.channel_id.is_none() {
            "Łączenie z kanałem…".to_owned()
        } else {
            format!("Napisz na #{}…   (Tab — emotki, @ — nicki)", tab.login)
        };

        let emote_list = tab
            .emote_list
            .get_or_insert_with(|| store.map_for(tab.channel_id.as_deref().unwrap_or_default()).sorted_for_picker());

        let inner = Margin::symmetric(10, 6);
        let frame = theme::glass(26, Margin { left: GAP, right: GAP, top: 4, bottom: 2 }, inner);
        let mut sent = None;
        let panel = egui::TopBottomPanel::bottom("composer").frame(frame).show_separator_line(false).show(ctx, |ui| {
            theme::with_sheen(ui, inner, 26.0, |ui| {
                sent = tab.composer.show(ui, enabled, &hint, emote_list, || tab.chat.recent_chatters(300));
            });
        });

        // Poświata wokół pigułki, gdy piszesz — rysowana pod panelem, więc
        // prześwituje przez szkło i rozlewa się na zewnątrz.
        let focus = ctx.animate_bool_with_time(egui::Id::new("composer_focus"), tab.composer.is_focused(), 0.25);
        if focus > 0.0 {
            let rect = panel.response.rect - Margin { left: GAP, right: GAP, top: 4, bottom: 2 };
            let breathe = if animate { (ctx.input(|i| i.time) as f32 * 2.2).sin() * 0.08 } else { 0.0 };
            let glow = egui::Shadow { offset: [0, 0], blur: 26, spread: 1, color: alpha(VIOLET, (0.42 + breathe) * focus) };
            ctx.layer_painter(egui::LayerId::background()).add(glow.as_shape(rect, 26.0));
        }

        let Some(text) = sent else { return };
        if demo {
            if let Some(map) = &self.demo_emotes {
                let tab = &mut self.tabs[self.active];
                tab.chat.push_message(Box::new(crate::demo::echo(&text, map)));
            }
        } else if let Some(channel) = self.tabs[self.active].channel_ref() {
            self.send(Command::SendMessage { channel, text });
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let animate = self.animations;
        let logged_in = self.logged_in();
        let global_emotes = self.global_emote_count;
        let frame = egui::Frame::NONE.inner_margin(Margin { left: 18, right: 18, top: 4, bottom: 8 });
        egui::TopBottomPanel::bottom("status_bar").frame(frame).show_separator_line(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    ui.label(RichText::new("brak otwartych kanałów").font(theme::mono(11.0)).color(TEXT_FAINT));
                    return;
                };

                if tab.channel_id.is_some() {
                    theme::live_dot(ui, OK, animate);
                    ui.label(RichText::new(format!("#{}", tab.login)).font(theme::mono(11.5)).color(TEXT_DIM));
                    let rate = tab.chat.rate();
                    theme::activity_bars(ui, rate, animate).on_hover_text("Aktywność czatu");
                    ui.label(RichText::new(format!("{:.0}/min", rate * 60.0)).font(theme::mono(11.0)).color(TEXT_FAINT));
                } else {
                    theme::live_dot(ui, WARN, animate);
                    ui.label(RichText::new(format!("łączenie z #{}…", tab.login)).font(theme::mono(11.5)).color(WARN));
                }

                if tab.channel_id.is_some() && logged_in {
                    ui.label(RichText::new("·").color(TEXT_FAINT));
                    if tab.can_moderate {
                        ui.label(theme::tag("moderator", OK));
                    } else {
                        ui.label(theme::tag("widz", TEXT_FAINT));
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("7TV {global_emotes} · {}", tab.channel_emote_count))
                            .font(theme::mono(11.0))
                            .color(TEXT_FAINT),
                    )
                    .on_hover_text("Emotki 7TV: globalne · kanałowe");
                    if !tab.chat.is_at_bottom() {
                        ui.label(theme::tag("⏸ pauza", VIOLET_SOFT)).on_hover_text("Przewijanie wstrzymane — czytasz historię");
                    }
                });
            });
        });
    }

    // ── Środek ─────────────────────────────────────────────────────────

    fn lobby(&mut self, ui: &mut egui::Ui) {
        let animate = self.animations;
        let time = ui.input(|i| i.time) as f32;
        let logged_out = matches!(self.auth, AuthState::LoggedOut);
        let mut login = false;

        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.16).max(12.0));

            // Logo unosi się delikatnie, a pod nim pulsuje fioletowa poświata.
            let float = if animate { (time * 1.1).sin() * 6.0 } else { 0.0 };
            let width = (ui.available_width() * 0.75).min(380.0);
            let size = vec2(width, width * 550.0 / 900.0);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            let rect = rect.translate(vec2(0.0, float));
            let pulse = if animate { (time * 1.6).sin() * 0.5 + 0.5 } else { 0.5 };
            let glow_painter = ui.ctx().layer_painter(egui::LayerId::background());
            theme::radial_glow(&glow_painter, rect.center(), width * (0.75 + 0.05 * pulse), alpha(VIOLET, 0.22 + 0.10 * pulse));
            Image::new(egui::include_image!("../assets/logo.png")).paint_at(ui, rect);

            ui.add_space(8.0);
            theme::flowing_text(ui, "czat Twitcha · moderacja · 7TV", theme::brand(18.0), animate);
            ui.add_space(4.0);
            ui.label(RichText::new("Otwórz kilka kanałów naraz — każdy w osobnej zakładce.").color(TEXT_DIM));
            ui.add_space(18.0);

            ui.horizontal(|ui| {
                let buttons_width = if logged_out { 380.0 } else { 190.0 };
                ui.add_space(((ui.available_width() - buttons_width) / 2.0).max(0.0));
                if theme::accent_button(ui, "+  Dołącz do kanału", true).clicked() {
                    self.adding_tab = true;
                }
                if logged_out && ui.add(egui::Button::new("Zaloguj przez Twitch").min_size(vec2(170.0, 32.0))).clicked() {
                    login = true;
                }
            });

            ui.add_space(22.0);
            for (text, level) in &self.lobby_log {
                let color = match level {
                    SystemLevel::Info => TEXT_FAINT,
                    SystemLevel::Error => ERR,
                };
                ui.label(RichText::new(text).size(12.5).color(color));
            }
        });

        if login {
            self.send(Command::Login);
        }
    }

    /// Okno z kodem urządzenia, widoczne w trakcie logowania.
    fn login_window(&mut self, ctx: &egui::Context) {
        let AuthState::Pending { user_code, verification_uri, expires_at } = &self.auth else { return };
        let total = 1_800.0_f32;
        let remaining = expires_at.saturating_duration_since(Instant::now());
        let (user_code, verification_uri) = (user_code.clone(), verification_uri.clone());
        let animate = self.animations;
        let mut cancel = false;

        egui::Window::new("login")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_width(330.0);
                ui.vertical_centered(|ui| {
                    ui.add(Image::new(egui::include_image!("../assets/logo.png")).fit_to_exact_size(vec2(150.0, 92.0)));
                    ui.label(RichText::new("Logowanie przez Twitch").font(theme::brand(20.0)));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Potwierdź w przeglądarce, że widzisz ten kod:").color(TEXT_DIM));
                    ui.add_space(12.0);

                    // Kod w szklanej ramce z pulsującą poświatą.
                    let galley = ui.painter().layout_no_wrap(user_code.clone(), theme::mono(30.0), TEXT);
                    let size = galley.size() + vec2(40.0, 22.0);
                    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                    let pulse = if animate { (ui.input(|i| i.time) as f32 * 2.0).sin() * 0.5 + 0.5 } else { 0.5 };
                    let glow = egui::Shadow { offset: [0, 0], blur: 24, spread: 0, color: alpha(VIOLET, 0.25 + 0.25 * pulse) };
                    ui.painter().add(glow.as_shape(rect, 14.0));
                    ui.painter().rect_filled(rect, 14.0, Color32::from_rgba_premultiplied(26, 20, 42, 255));
                    ui.painter().rect_stroke(rect, 14.0, Stroke::new(1.0_f32, alpha(VIOLET_SOFT, 0.5)), egui::StrokeKind::Inside);
                    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, TEXT);

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.add_space(40.0);
                        if ui.button("📋 Kopiuj kod").clicked() {
                            ui.ctx().copy_text(user_code.clone());
                        }
                        if ui.button("🔗 Otwórz stronę").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&verification_uri));
                        }
                    });

                    // Pasek odliczania, który kurczy się do wygaśnięcia kodu.
                    ui.add_space(12.0);
                    let fraction = (remaining.as_secs_f32() / total).clamp(0.0, 1.0);
                    let (bar, _) = ui.allocate_exact_size(vec2(260.0, 4.0), Sense::hover());
                    ui.painter().rect_filled(bar, 2.0, white(16));
                    let mut filled = bar;
                    filled.set_width(bar.width() * fraction);
                    ui.painter().rect_filled(filled, 2.0, VIOLET);
                    ui.label(
                        RichText::new(format!("kod wygaśnie za {}:{:02}", remaining.as_secs() / 60, remaining.as_secs() % 60))
                            .font(theme::mono(11.0))
                            .color(TEXT_FAINT),
                    );
                    ui.add_space(6.0);
                    if ui.button("Anuluj").clicked() {
                        cancel = true;
                    }
                });
            });

        ctx.request_repaint_after(if animate { ANIMATION_FRAME } else { Duration::from_secs(1) });
        if cancel {
            self.send(Command::CancelLogin);
        }
    }
}

/// Awatar w okręgu z fioletowym pierścieniem, który powoli „oddycha”.
fn avatar_chip(ui: &mut egui::Ui, login: &str, avatar_url: Option<&str>, animate: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(login.to_owned(), theme::semi(13.5), TEXT);
    let size = vec2(30.0 + 8.0 + galley.size().x + 14.0, 36.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = ui.ctx().animate_bool_with_time(response.id, response.hovered(), 0.15);
    let painter = ui.painter();
    painter.rect_filled(rect, 18.0, white((8.0 + 10.0 * hover) as u8));
    painter.rect_stroke(rect, 18.0, Stroke::new(1.0_f32, white(20)), egui::StrokeKind::Inside);

    let center = egui::pos2(rect.left() + 18.0, rect.center().y);
    let pulse = if animate { (ui.input(|i| i.time) as f32 * 1.8).sin() * 0.5 + 0.5 } else { 0.5 };
    painter.circle_filled(center, 15.5 + pulse, alpha(VIOLET, 0.25 + 0.2 * pulse));
    painter.circle_stroke(center, 14.5, Stroke::new(1.5_f32, VIOLET_SOFT));
    let avatar_rect = egui::Rect::from_center_size(center, vec2(26.0, 26.0));
    match avatar_url {
        Some(url) => Image::new(url.to_owned()).corner_radius(13.0).paint_at(ui, avatar_rect),
        None => {
            painter.circle_filled(center, 13.0, VIOLET_DEEP);
            let initial = login.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            painter.text(center, Align2::CENTER_CENTER, initial, theme::brand(14.0), TEXT);
        }
    }
    // Zielona kropka „online”.
    let dot = center + vec2(10.0, 10.0);
    painter.circle_filled(dot, 4.0, Color32::from_rgb(0x12, 0x0f, 0x1a));
    painter.circle_filled(dot, 2.8, OK);

    painter.galley(egui::pos2(rect.left() + 38.0, rect.center().y - galley.size().y / 2.0), galley, TEXT);
    response
}

fn add_tab_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(response.id, response.hovered(), 0.15);
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 15.0, alpha(VIOLET, 0.12 + 0.25 * hover));
    painter.circle_stroke(rect.center(), 15.0, Stroke::new(1.0_f32, alpha(VIOLET_SOFT, 0.35 + 0.4 * hover)));
    let c = rect.center();
    // Plus obraca się o 90° przy najechaniu.
    let angle = hover * std::f32::consts::FRAC_PI_2;
    let arm = vec2(angle.cos(), angle.sin()) * 6.0;
    let stroke = Stroke::new(1.8_f32, TEXT);
    painter.line_segment([c - arm, c + arm], stroke);
    painter.line_segment([c - arm.rot90(), c + arm.rot90()], stroke);
    response
}

impl eframe::App for ChatApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Przezroczysty kolor czyszczenia: przy szkle systemowym prześwituje
        // rozmyty pulpit; bez niego i tak zamalowujemy całe okno tłem.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let animate = self.animations && focused;
        self.backdrop.paint(ctx, animate, self.system_glass_active);

        self.handle_events(ctx);
        self.header(ctx);
        // Kolejność paneli dolnych: pierwszy dodany jest najniżej.
        self.status_bar(ctx);
        self.composer_bar(ctx);

        let demo = self.is_demo();
        let logged_in = self.logged_in();
        let my_login = self.my_login();
        let (mod_mode, show_timestamps, animations) = (self.mod_mode, self.show_timestamps, self.animations);

        let inner = Margin::same(4);
        let frame = theme::glass(22, Margin { left: GAP, right: GAP, top: 4, bottom: 4 }, inner);
        let actions = egui::CentralPanel::default()
            .frame(frame)
            .show(ctx, |ui| {
                theme::with_sheen(ui, inner, 22.0, |ui| {
                    let Some(tab) = self.tabs.get_mut(self.active) else {
                        self.lobby(ui);
                        return Vec::new();
                    };
                    let opts = RenderOptions {
                        mod_mode,
                        can_moderate: demo || (logged_in && tab.can_moderate),
                        emote_height: 28.0,
                        show_timestamps,
                        animate: animations,
                        my_login: my_login.as_deref(),
                        badges: &tab.merged_badges,
                    };
                    tab.chat.show(ui, &opts)
                })
            })
            .inner;

        self.login_window(ctx);

        if !demo {
            for action in actions {
                self.send(action);
            }
        }

        // Płynąca zorza i pulsujące akcenty: ~30 kl./s tylko przy aktywnym oknie.
        if animate {
            ctx.request_repaint_after(ANIMATION_FRAME);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Zapisujemy tylko ustawienia UI. Plik ustawień eframe to jawny tekst,
        // więc tokeny NIGDY tu nie trafiają — są wyłącznie w sejfie systemowym.
        if self.is_demo() {
            return;
        }
        let channels: Vec<&str> = self.tabs.iter().map(|t| t.login.as_str()).collect();
        eframe::set_value(storage, STORAGE_CHANNELS, &channels);
        eframe::set_value(storage, STORAGE_ACTIVE, &self.active);
        eframe::set_value(storage, STORAGE_MOD_MODE, &self.mod_mode);
        eframe::set_value(storage, STORAGE_TIMESTAMPS, &self.show_timestamps);
        eframe::set_value(storage, STORAGE_ANIMATIONS, &self.animations);
        // Usuń klucz z wersji jednokanałowej, żeby migracja nie powtarzała się.
        eframe::set_value(storage, "last_channel", &String::new());
    }
}

impl Drop for ChatApp {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(rt) = self.runtime.take() {
            // Nie czekamy na zamknięcie gniazd — okno ma zniknąć natychmiast.
            rt.shutdown_background();
        }
    }
}

fn format_duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3_600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3_600),
        s => format!("{} dni", s / 86_400),
    }
}

/// Krótki opis alertu dla aplikacji PC.
fn alert_line(a: &crate::events::ChatAlert) -> String {
    use crate::events::AlertKind as K;
    let n = &a.user_name;
    match &a.kind {
        K::Bits { amount } => format!("{n}: {amount} bitsów"),
        K::Sub { months, resub: true, .. } => format!("{n} subskrybuje od {months} mies."),
        K::Sub { .. } => format!("{n} subskrybuje!"),
        K::GiftSub { recipient, anonymous, .. } => format!("{} podarował suba dla {recipient}", if *anonymous { "Anonim" } else { n }),
        K::MassGift { count, anonymous, .. } => format!("{} rozdaje {count} subów", if *anonymous { "Anonim" } else { n }),
        K::Raid { viewers, .. } => format!("{n} robi raid z {viewers} widzami"),
        K::BitsBadge { threshold } => format!("{n} zdobywa odznakę {threshold} bitsów"),
        K::GiftUpgrade { .. } => format!("{n} kontynuuje suba"),
        K::NewChatter => format!("{n} jest tu pierwszy raz"),
        K::Follow => format!("{n} obserwuje kanał"),
        K::WatchStreak { streams } => format!("{n} ogląda już {streams} streamów z rzędu"),
        K::Redemption { reward, .. } => format!("{n} odbiera nagrodę: {reward}"),
        K::Vip { greeting, .. } => greeting.clone(),
    }
}
