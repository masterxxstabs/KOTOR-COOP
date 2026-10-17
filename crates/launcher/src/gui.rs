//! Launcher window (egui). Network and disk work runs on worker threads and
//! reports back over a channel so the UI never freezes.

use crate::settings::Settings;
use crate::{game, servers, update};
use eframe::egui;
use kotor_protocol::master::ServerListing;
use std::path::PathBuf;
use std::sync::mpsc;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Servers,
    Mod,
    Settings,
}

enum Msg {
    Servers(Result<Vec<(ServerListing, Option<u64>)>, String>),
    Log(String),
    UpdateChecked(Result<(String, usize, usize), String>),
    UpdateDone(Result<(), String>),
    Launched(Result<String, String>),
}

pub struct App {
    settings: Settings,
    tab: Tab,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    servers: Vec<(ServerListing, Option<u64>)>,
    selected: Option<usize>,
    direct_address: String,
    password: String,
    busy: Option<String>,
    status: String,
    log: Vec<String>,
    installed_version: Option<String>,
    available: Option<(String, usize, usize)>,
    game_dir_text: String,
}

pub fn run(settings: Settings) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("KOTOR Co-op Launcher")
            .with_inner_size([860.0, 560.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "KOTOR Co-op Launcher",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc, settings)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

impl App {
    fn new(cc: &eframe::CreationContext, mut settings: Settings) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        if settings.game_dir.is_none() {
            settings.game_dir = game::detect().into_iter().next();
            let _ = settings.save();
        }
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            game_dir_text: settings
                .game_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            direct_address: settings.last_server.clone(),
            settings,
            tab: Tab::Servers,
            tx,
            rx,
            servers: Vec::new(),
            selected: None,
            password: String::new(),
            busy: None,
            status: String::new(),
            log: Vec::new(),
            installed_version: None,
            available: None,
        };
        app.refresh_installed();
        app.refresh_servers();
        app.check_updates();
        app
    }

    fn game_dir(&self) -> Option<PathBuf> {
        self.settings
            .game_dir
            .clone()
            .filter(|g| game::is_game_dir(g))
    }

    fn refresh_installed(&mut self) {
        self.installed_version = self
            .game_dir()
            .and_then(|g| update::Installed::load(&g).version);
    }

    fn spawn(&mut self, label: &str, job: impl FnOnce(mpsc::Sender<Msg>) + Send + 'static) {
        self.busy = Some(label.to_owned());
        let tx = self.tx.clone();
        std::thread::spawn(move || job(tx));
    }

    fn refresh_servers(&mut self) {
        let master = self.settings.master_url.clone();
        self.spawn("Fetching servers...", move |tx| {
            let res = servers::fetch(&master)
                .map_err(|e| e.to_string())
                .map(|list| {
                    let pings: Vec<_> = list
                        .iter()
                        .map(|s| {
                            let a = s.address.clone();
                            std::thread::spawn(move || servers::ping(&a))
                        })
                        .collect();
                    list.into_iter()
                        .zip(pings)
                        .map(|(s, p)| (s, p.join().ok().flatten()))
                        .collect()
                });
            let _ = tx.send(Msg::Servers(res));
        });
    }

    fn check_updates(&mut self) {
        let Some(g) = self.game_dir() else { return };
        let (url, key) = (
            self.settings.update_url.clone(),
            self.settings.update_public_key.clone(),
        );
        self.spawn("Checking for updates...", move |tx| {
            let res = update::check(&*update::source_for(&url), &g, Some(&key))
                .map(|c| (c.manifest.version, c.needed.len(), c.stale.len()))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::UpdateChecked(res));
        });
    }

    fn run_update(&mut self) {
        let Some(g) = self.game_dir() else {
            self.status = "Set your KOTOR folder in Settings first.".into();
            return;
        };
        let (url, key) = (
            self.settings.update_url.clone(),
            self.settings.update_public_key.clone(),
        );
        self.spawn("Updating...", move |tx| {
            let src = update::source_for(&url);
            let res = update::check(&*src, &g, Some(&key)).and_then(|c| {
                let tx2 = tx.clone();
                update::apply(&*src, &g, &c, &mut |l| {
                    let _ = tx2.send(Msg::Log(l));
                })
            });
            let _ = tx.send(Msg::UpdateDone(res.map_err(|e| format!("{e:#}"))));
        });
    }

    fn join(&mut self, address: String) {
        let Some(g) = self.game_dir() else {
            self.status = "Set your KOTOR folder in Settings first.".into();
            self.tab = Tab::Settings;
            return;
        };
        if self.settings.player_name.trim().is_empty() {
            self.status = "Choose a player name in Settings first.".into();
            self.tab = Tab::Settings;
            return;
        }
        self.settings.last_server = address.clone();
        let _ = self.settings.save();
        let settings = self.settings.clone();
        let password = Some(self.password.clone()).filter(|p| !p.is_empty());
        let version = self.installed_version.clone();
        self.spawn("Starting KOTOR...", move |tx| {
            let res = game::write_session(
                &g,
                &game::Session {
                    server: &address,
                    player_name: settings.player_name.trim(),
                    player_token: &settings.player_token,
                    password: password.as_deref(),
                    mod_version: version.as_deref(),
                },
            )
            .and_then(|_| game::launch(&g))
            .map(|_| format!("KOTOR started, joining {address}"))
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Launched(res));
        });
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.busy = None;
            match msg {
                Msg::Servers(Ok(list)) => {
                    self.status = format!("{} server(s) online", list.len());
                    self.servers = list;
                    self.selected = None;
                }
                Msg::Servers(Err(e)) => self.status = format!("Server list unavailable: {e}"),
                Msg::Log(l) => {
                    self.busy = Some("Updating...".into());
                    self.log.push(l);
                }
                Msg::UpdateChecked(Ok(v)) => {
                    self.status = if v.1 + v.2 == 0 {
                        format!("Mod {} is up to date", v.0)
                    } else {
                        format!("Update {} available ({} file(s))", v.0, v.1 + v.2)
                    };
                    self.available = Some(v);
                }
                Msg::UpdateChecked(Err(e)) => self.status = format!("Update check failed: {e}"),
                Msg::UpdateDone(r) => {
                    self.status = match r {
                        Ok(()) => "Update installed".into(),
                        Err(e) => format!("Update failed: {e}"),
                    };
                    self.log.push(self.status.clone());
                    self.refresh_installed();
                    self.available = self.available.take().map(|(v, _, _)| (v, 0, 0));
                }
                Msg::Launched(r) => {
                    self.status = r.unwrap_or_else(|e| format!("Could not start the game: {e}"))
                }
            }
        }
    }

    fn servers_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.refresh_servers();
            }
            ui.separator();
            ui.label("Direct connect:");
            ui.add(
                egui::TextEdit::singleline(&mut self.direct_address)
                    .hint_text("host:27700")
                    .desired_width(180.0),
            );
            ui.label("Password:");
            ui.add(
                egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .desired_width(100.0),
            );
            if ui.button("Connect").clicked() && !self.direct_address.trim().is_empty() {
                let a = self.direct_address.trim().to_owned();
                self.join(a);
            }
        });
        ui.separator();

        let mine = kotor_protocol::PROTOCOL_VERSION;
        let mut join = None;
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - 40.0)
            .show(ui, |ui| {
                egui::Grid::new("servers")
                    .striped(true)
                    .num_columns(6)
                    .min_col_width(60.0)
                    .show(ui, |ui| {
                        for h in ["Name", "Players", "Area", "Ping", "Version", ""] {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for (i, (s, ping)) in self.servers.iter().enumerate() {
                            let lock = if s.password_protected { "🔒 " } else { "" };
                            if ui
                                .selectable_label(
                                    self.selected == Some(i),
                                    format!("{lock}{}", s.name),
                                )
                                .on_hover_text(&s.description)
                                .clicked()
                            {
                                self.selected = Some(i);
                            }
                            ui.label(format!("{}/{}", s.players, s.max_players));
                            ui.label(s.module.as_deref().unwrap_or("new campaign"));
                            ui.label(
                                ping.map(|p| format!("{p} ms"))
                                    .unwrap_or_else(|| "-".into()),
                            );
                            if s.protocol_version == mine {
                                ui.label(&s.mod_version);
                            } else {
                                ui.colored_label(
                                    egui::Color32::LIGHT_RED,
                                    format!("{} (incompatible)", s.mod_version),
                                );
                            }
                            if ui
                                .add_enabled(
                                    s.protocol_version == mine && s.players < s.max_players,
                                    egui::Button::new("Join"),
                                )
                                .clicked()
                            {
                                join = Some(s.address.clone());
                            }
                            ui.end_row();
                        }
                    });
                if self.servers.is_empty() {
                    ui.label("No servers listed. Host one with kotor-server, or connect directly.");
                }
            });
        if let Some(a) = join {
            self.join(a);
        }
    }

    fn mod_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(format!(
            "Installed: {}",
            self.installed_version.as_deref().unwrap_or("not installed")
        ));
        if let Some((v, n, s)) = &self.available {
            ui.label(format!("Latest: {v} ({} file(s) to change)", n + s));
        }
        if self.settings.update_public_key.trim().is_empty() {
            ui.colored_label(
                egui::Color32::YELLOW,
                "No update signing key set: downloads are hash-checked but not signature-verified.",
            );
        }
        ui.horizontal(|ui| {
            let idle = self.busy.is_none();
            if ui
                .add_enabled(idle, egui::Button::new("Check for updates"))
                .clicked()
            {
                self.check_updates();
            }
            if ui
                .add_enabled(idle, egui::Button::new("Install / Update"))
                .clicked()
            {
                self.log.clear();
                self.run_update();
            }
            if ui
                .add_enabled(idle, egui::Button::new("Uninstall (restore game)"))
                .clicked()
            {
                if let Some(g) = self.game_dir() {
                    self.log.clear();
                    let mut lines = Vec::new();
                    let r = update::uninstall(&g, &mut |l| lines.push(l));
                    self.log.extend(lines);
                    self.status = match r {
                        Ok(()) => "Mod removed".into(),
                        Err(e) => format!("Uninstall failed: {e:#}"),
                    };
                    self.refresh_installed();
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for l in &self.log {
                    ui.monospace(l);
                }
            });
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("settings")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Player name");
                ui.add(egui::TextEdit::singleline(&mut self.settings.player_name).char_limit(24));
                ui.end_row();
                ui.label("KOTOR folder");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.game_dir_text).desired_width(380.0),
                    );
                    if ui.button("Detect").clicked() {
                        match game::detect().into_iter().next() {
                            Some(p) => self.game_dir_text = p.display().to_string(),
                            None => {
                                self.status =
                                    "KOTOR not found automatically; paste the folder path.".into()
                            }
                        }
                    }
                });
                ui.end_row();
                ui.label("Server list URL");
                ui.text_edit_singleline(&mut self.settings.master_url);
                ui.end_row();
                ui.label("Update URL");
                ui.text_edit_singleline(&mut self.settings.update_url);
                ui.end_row();
                ui.label("Update public key");
                ui.text_edit_singleline(&mut self.settings.update_public_key);
                ui.end_row();
            });
        ui.add_space(8.0);
        if ui.button("Save").clicked() {
            let p = PathBuf::from(self.game_dir_text.trim());
            if !self.game_dir_text.trim().is_empty() && !game::is_game_dir(&p) {
                self.status = "That folder has no swkotor.exe/chitin.key.".into();
                return;
            }
            self.settings.game_dir = Some(p).filter(|p| !p.as_os_str().is_empty());
            self.status = match self.settings.save() {
                Ok(()) => "Settings saved".into(),
                Err(e) => format!("Could not save settings: {e}"),
            };
            self.refresh_installed();
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if self.busy.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("KOTOR Co-op");
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Servers, "Servers");
                ui.selectable_value(&mut self.tab, Tab::Mod, "Mod & Patches");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
            });
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(b) = &self.busy {
                    ui.spinner();
                    ui.label(b);
                } else {
                    ui.label(&self.status);
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Servers => self.servers_tab(ui),
            Tab::Mod => self.mod_tab(ui),
            Tab::Settings => self.settings_tab(ui),
        });
    }
}
