//! PyroMirror launcher: one window to connect to a host or to share this computer.
//!
//! The streaming itself is done by `pyromirror-client` and `pyromirror-server`, which this
//! program starts with the chosen settings and watches. Everything is drawn by egui, so the
//! window looks the same on every platform.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod agent;
mod autostart;
mod config;
mod daemon;
mod notify;
mod process;
mod state;
mod tray;

use std::path::PathBuf;

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};

use pyromirror_proto::auth::{self, TokenStore};

use config::{Computer, Config, SettingsPage, Tab};
use daemon::Daemon;
use process::{Level, LogLine, Process};
use state::{client_state, host_state, ClientState, HostState};

const BG: Color32 = Color32::from_rgb(0x12, 0x15, 0x1c);
const CARD: Color32 = Color32::from_rgb(0x1b, 0x20, 0x2b);
const FIELD: Color32 = Color32::from_rgb(0x26, 0x2c, 0x3a);
const FIELD_HOVER: Color32 = Color32::from_rgb(0x30, 0x38, 0x49);
const TRACK: Color32 = Color32::from_rgb(0x10, 0x13, 0x19);
const SEGMENT: Color32 = Color32::from_rgb(0x3a, 0x43, 0x57);
const BORDER: Color32 = Color32::from_rgb(0x2e, 0x35, 0x45);
const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe9, 0xef);
const MUTED: Color32 = Color32::from_rgb(0x8b, 0x93, 0xa3);
const ACCENT: Color32 = Color32::from_rgb(0xff, 0x7a, 0x2f);
const ACCENT_HOVER: Color32 = Color32::from_rgb(0xff, 0x91, 0x52);
const GREEN: Color32 = Color32::from_rgb(0x3e, 0xcf, 0x8e);
const YELLOW: Color32 = Color32::from_rgb(0xf2, 0xc1, 0x4e);
const RED: Color32 = Color32::from_rgb(0xf0, 0x5d, 0x5e);

fn main() -> eframe::Result {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    // `--screenshot <file> [host|connect]` renders one frame to a PPM image and exits; it exists
    // so the look of the window can be checked without a person in front of it.
    let args: Vec<String> = std::env::args().collect();
    let screenshot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1).map(|path| (PathBuf::from(path), args.get(i + 2).cloned())));

    // `--dump-icons <dir>` writes the tray icon in each state as PPM images, for checking them.
    if let Some(dir) = args.iter().position(|a| a == "--dump-icons").and_then(|i| args.get(i + 1)) {
        use tray::Indicator::*;
        for (name, indicator) in [("off", Off), ("on", On), ("connected", Connected), ("attention", Attention)] {
            for size in [16u32, 32, 64] {
                let rgba = tray::icon_rgba(size, indicator);
                // Composite on a taskbar-like grey so transparency is visible.
                let mut ppm = format!("P6\n{size} {size}\n255\n").into_bytes();
                ppm.extend(rgba.chunks_exact(4).flat_map(|p| [0, 1, 2].map(|c| ((p[c] as u32 * p[3] as u32 + 60 * (255 - p[3] as u32)) / 255) as u8)));
                let _ = std::fs::write(format!("{dir}/{name}-{size}.ppm"), ppm);
            }
        }
        return Ok(());
    }

    // Background mode: tray icon and optional auto-sharing, no window. This is what runs at login.
    if args.iter().any(|a| a == "--background") {
        agent::run();
        return Ok(());
    }

    // One window is enough; a second start (e.g. from the tray) just returns.
    let window = Daemon::window();
    if screenshot.is_none() && !window.claim() {
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PyroMirror")
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap_or_default())
            .with_app_id("pyromirror")
            .with_inner_size([460.0, 720.0])
            .with_min_inner_size([440.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "PyroMirror",
        options,
        Box::new(move |cc| {
            apply_theme(&cc.egui_ctx);
            Ok(Box::new(App::new(screenshot)))
        }),
    )
}

/// One fixed dark theme, independent of the system's, so every platform shows the same window.
fn apply_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.slider_width = 190.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(TEXT);
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.extreme_bg_color = FIELD;
        v.faint_bg_color = CARD;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;

        let radius = CornerRadius::same(6);
        for (widget, fill) in [
            (&mut v.widgets.noninteractive, CARD),
            (&mut v.widgets.inactive, FIELD),
            (&mut v.widgets.hovered, FIELD_HOVER),
            (&mut v.widgets.active, FIELD_HOVER),
            (&mut v.widgets.open, FIELD_HOVER),
        ] {
            widget.bg_fill = fill;
            widget.weak_bg_fill = fill;
            widget.corner_radius = radius;
            widget.bg_stroke = Stroke::NONE;
            widget.fg_stroke = Stroke::new(1.0, TEXT);
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        // Without an outline an unticked checkbox disappears into the card behind it.
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, MUTED.gamma_multiply(0.6));
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    });
}

fn pairing_file(name: &str) -> PathBuf {
    auth::config_dir().unwrap_or_else(std::env::temp_dir).join(name)
}

/// This computer's address on the network it would use to reach others. No packet is sent:
/// connecting a UDP socket only makes the OS pick a route.
fn local_address() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
}

/// "Add a computer": takes an address, pairs with the computer there, and saves it.
#[derive(Default)]
struct AddDialog {
    address: String,
    /// `pyromirror-client --pair-only`, while it is talking to the other computer.
    pairing: Option<Process>,
    code: String,
    codes_sent: usize,
    error: Vec<LogLine>,
}

struct App {
    config: Config,
    saved: Config,
    /// The sharing server. It runs detached, so the tray agent can keep it going (and control
    /// it) while this window is closed; what we show is read from its pid and log files.
    server: Daemon,
    server_running: bool,
    server_log: Vec<LogLine>,
    server_polled: std::time::Instant,
    /// True between starting the server and noticing it gone, to tell a crash from "off".
    sharing_expected: bool,
    agent: Daemon,
    window: Daemon,
    /// Notifies about connections while the tray agent is not running to do it.
    announcer: notify::Announcer,
    /// A running `pyromirror-server --check-permissions`, started by ticking "share automatically".
    permission_check: Option<Process>,
    permission_error: Vec<LogLine>,
    /// The pairing step of a connection (`pyromirror-client --pair-only`). It runs as our child
    /// so that it can ask for a code through this window.
    client: Option<Process>,
    /// The remote desktop window itself. Like the server it runs detached, so it stays open
    /// when this window is closed.
    viewer: Daemon,
    viewer_running: bool,
    viewer_log: Vec<LogLine>,
    viewer_expected: bool,
    /// Last lines of a process that stopped on its own with an error.
    server_error: Vec<LogLine>,
    client_error: Vec<LogLine>,
    local_address: Option<String>,
    /// The computer the viewer is (being) connected to: name and address.
    target: (String, String),
    /// The "Add a computer" dialog, while it is open.
    add: Option<AddDialog>,
    /// Index of the computer whose "Remove" is waiting for confirmation.
    confirm_remove: Option<usize>,
    /// Pairing code being typed into the prompt on the Connect tab.
    pairing_code: String,
    /// How many pairing prompts of the current connection have been answered, so that an
    /// answered prompt shows "checking" instead of asking again.
    codes_sent: usize,
    was_pairing: bool,
    /// Computers allowed to connect to this one, and computers this one can connect to. Both
    /// are re-read from disk regularly, because the server and viewer change them.
    paired_clients: TokenStore,
    paired_hosts: TokenStore,
    stores_read: std::time::Instant,
    /// The entry whose "Forget" was clicked and is waiting for confirmation.
    confirm_forget: Option<auth::Id>,
    screenshot: Option<PathBuf>,
    scroll_to_bottom: bool,
    /// Screenshot mode only: pretend the server is in this state.
    demo: Option<HostState>,
    frames: u32,
}

impl App {
    fn new(screenshot: Option<(PathBuf, Option<String>)>) -> Self {
        let mut config = Config::load();
        let saved = config.clone();
        let mut demo = None;
        let open_add = screenshot.as_ref().is_some_and(|(_, tab)| tab.as_deref() == Some("connect-add"));
        let scroll_to_bottom = screenshot.as_ref().is_some_and(|(_, tab)| tab.as_deref() == Some("host-bottom"));
        let screenshot = screenshot.map(|(path, tab)| {
            let tab_name = tab.as_deref().unwrap_or("connect");
            config.tab = if tab_name.starts_with("host") {
                Tab::Host
            } else if tab_name.starts_with("settings") {
                Tab::Settings
            } else {
                Tab::Connect
            };
            config.settings_page = match tab_name {
                "settings-connecting" => SettingsPage::Connecting,
                "settings-general" => SettingsPage::General,
                _ => SettingsPage::Sharing,
            };
            demo = match tab.as_deref() {
                Some("host-ready") => Some(HostState::Ready),
                Some("host-serving") => Some(HostState::Serving("caio-laptop (192.168.1.7)".into())),
                Some("host-pairing") => Some(HostState::PairingRequest { name: "caio-laptop".into(), code: "482 913".into() }),
                _ => None,
            };
            path
        });
        let mut app = Self {
            config,
            saved,
            server: Daemon::server(),
            server_running: false,
            server_log: Vec::new(),
            server_polled: std::time::Instant::now() - std::time::Duration::from_secs(60),
            sharing_expected: false,
            agent: Daemon::agent(),
            window: Daemon::window(),
            announcer: notify::Announcer::default(),
            permission_check: None,
            permission_error: Vec::new(),
            client: None,
            viewer: Daemon::viewer(),
            viewer_running: false,
            viewer_log: Vec::new(),
            viewer_expected: false,
            server_error: Vec::new(),
            client_error: Vec::new(),
            local_address: local_address(),
            target: (String::new(), String::new()),
            add: open_add.then(AddDialog::default),
            confirm_remove: None,
            pairing_code: String::new(),
            codes_sent: 0,
            was_pairing: false,
            paired_clients: TokenStore::load(pairing_file("paired-clients")),
            paired_hosts: TokenStore::load(pairing_file("paired-hosts")),
            stores_read: std::time::Instant::now(),
            confirm_forget: None,
            screenshot,
            scroll_to_bottom,
            demo,
            frames: 0,
        };
        // A viewer left open by an earlier launcher is still "the current connection".
        app.target = (app.config.last_target.name.clone(), app.config.last_target.address.clone());
        if app.screenshot.is_none() {
            if app.config.autostart && !autostart::is_set() {
                // The login entry was removed outside PyroMirror; go along with that.
                app.config.autostart = false;
                app.config.auto_share = false;
            } else if app.config.autostart {
                // Keep the entry pointing at this copy of the program, and the tray icon up.
                app.apply_autostart();
            }
        }
        app
    }

    /// The last few lines of a log, for showing why something failed.
    fn failure(log: &[LogLine]) -> Vec<LogLine> {
        let mut lines = log[log.len().saturating_sub(6)..].to_vec();
        if lines.is_empty() {
            lines.push(LogLine { level: Level::Error, text: "The program exited unexpectedly.".into() });
        }
        lines
    }

    /// Refreshes what we know about the server, and notices processes that ended by themselves,
    /// keeping what they said if they failed.
    fn reap(&mut self) {
        if self.server_polled.elapsed() >= std::time::Duration::from_millis(300) {
            self.server_polled = std::time::Instant::now();
            self.server_running = self.server.is_running();
            self.server_log = self.server.log();
            if !self.agent.is_running() && self.demo.is_none() {
                let state = if self.server_running { host_state(&self.server_log) } else { HostState::Stopped };
                self.announcer.observe(&state);
            }
            self.viewer_running = self.viewer.is_running();
            self.viewer_log = self.viewer.log();
            if self.viewer_running {
                self.viewer_expected = true;
            } else if self.viewer_expected {
                // Closed from its own window, or it failed.
                self.viewer_expected = false;
                if self.viewer_log.iter().any(|l| l.level == Level::Error) {
                    self.client_error = Self::failure(&self.viewer_log);
                }
            }
            if self.server_running {
                // Started by us or by the tray agent; either way a later exit is unexpected.
                self.sharing_expected = true;
            } else if self.sharing_expected {
                self.sharing_expected = false;
                if self.server_log.iter().any(|l| l.level == Level::Error) {
                    self.server_error = Self::failure(&self.server_log);
                }
            }
        }

        // The pairing step finished: on success, open the remote desktop.
        if let Some(success) = self.client.as_mut().and_then(|p| p.exited()) {
            let log = self.client.as_ref().map(|p| p.log()).unwrap_or_default();
            self.client = None;
            if success {
                // Keep the entry current: the host's name may have changed, and entries carried
                // over from older versions have no id yet.
                if let Some((id, name)) = log.iter().find_map(|l| l.text.strip_prefix("Host: ")).and_then(|r| r.split_once(' ')) {
                    let computer = Computer { name: name.to_owned(), address: self.target.1.clone(), id: id.to_owned() };
                    self.target.0 = computer.name.clone();
                    self.config.last_target = computer.clone();
                    self.config.remember(computer);
                }
                self.viewer.stop();
                match self.viewer.start(&self.config.client_args(&self.target.1)) {
                    Ok(()) => self.viewer_expected = true,
                    Err(e) => self.client_error.push(LogLine { level: Level::Error, text: format!("Could not open the remote desktop: {}", e) }),
                }
                self.server_polled -= std::time::Duration::from_secs(1);
            } else {
                self.client_error = Self::failure(&log);
            }
        }

        if let Some(success) = self.permission_check.as_mut().and_then(|p| p.exited()) {
            if success {
                self.config.auto_share = true;
            } else {
                self.permission_error = Self::failure(&self.permission_check.as_ref().map(|p| p.log()).unwrap_or_default());
            }
            self.permission_check = None;
        }
    }

    /// Applies the "start at login" setting: the login entry, and the tray agent right now.
    fn apply_autostart(&mut self) {
        if let Err(e) = autostart::set(self.config.autostart) {
            log::warn!("Could not change the startup entry: {}", e);
        }
        if self.config.autostart {
            if let Err(e) = self.agent.start(&["--background".to_owned()]) {
                log::warn!("Could not start the tray icon: {}", e);
            }
        } else {
            self.agent.stop();
            // Sharing at login needs the agent.
            self.config.auto_share = false;
        }
    }

    fn start(name: &str, args: &[String], ctx: &egui::Context, error: &mut Vec<LogLine>) -> Option<Process> {
        error.clear();
        match Process::spawn(name, args, ctx) {
            Ok(process) => Some(process),
            Err(e) => {
                error.push(LogLine { level: Level::Error, text: format!("Could not start {}: {}", name, e) });
                None
            }
        }
    }

    fn connect_tab(&mut self, ui: &mut egui::Ui) {
        if let Some(client) = &mut self.client {
            let log = client.log();
            let prompts = log.iter().filter(|l| l.text.starts_with("Pairing code needed")).count();
            let address = self.target.0.clone();
            let mut stop = false;

            match client_state(&log) {
                ClientState::CodeNeeded { wrong, revoked } if prompts > self.codes_sent => {
                    let detail = if wrong {
                        "That code was not right. Check the code shown on the other computer and try again."
                    } else if revoked {
                        "The other computer has removed this one from its paired computers, so the old pairing no longer works. To connect again, type the code it is showing now."
                    } else {
                        "The other computer is showing a pairing code. Type it here; this is only needed once."
                    };
                    let title = if revoked && !wrong { format!("Pair with {} again", address) } else { format!("Pair with {}", address) };
                    let submit = state_panel(ui, YELLOW, &title, detail, |ui| {
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut self.pairing_code)
                                .hint_text("123 456")
                                .font(egui::FontId::monospace(22.0))
                                .desired_width(f32::INFINITY)
                                .margin(Margin::symmetric(10, 9)),
                        );
                        if self.pairing_code.is_empty() && !field.has_focus() {
                            field.request_focus();
                        }
                        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let ready = self.pairing_code.chars().filter(|c| c.is_ascii_digit()).count() == 6;
                        let pair = ui.add_enabled_ui(ready, |ui| action_button(ui, "Pair", ButtonKind::Primary).clicked()).inner;
                        stop = ui.link("Cancel").clicked();
                        ready && (pair || enter)
                    });
                    if submit {
                        client.send_line(self.pairing_code.trim());
                        self.codes_sent = prompts;
                        self.pairing_code.clear();
                    }
                }
                // Connecting, or a code was just sent and is being checked.
                _ => {
                    let title = format!("Connecting to {}...", address);
                    stop = state_panel(ui, YELLOW, &title, "", |ui| action_button(ui, "Cancel", ButtonKind::Stop).clicked());
                }
            }
            if stop {
                self.client = None;
            }
        } else if self.viewer_running {
            // The remote desktop is its own program: it stays open if this window is closed.
            let name = self.target.0.clone();
            let streaming = client_state(&self.viewer_log) == ClientState::Connected;
            let (color, title, detail) = if streaming {
                (GREEN, format!("Connected to {}", name), "The remote desktop is open in its own window. You can close this window; the connection stays.")
            } else {
                (YELLOW, format!("Connecting to {}...", name), "")
            };
            let label = if streaming { "Disconnect" } else { "Cancel" };
            if state_panel(ui, color, &title, detail, |ui| action_button(ui, label, ButtonKind::Stop).clicked()) {
                self.viewer.stop();
                self.viewer_expected = false;
                self.server_polled -= std::time::Duration::from_secs(1);
            }
        } else {
            let mut connect_to = None;
            let mut remove = None;
            card(ui, |ui| {
                section(ui, "Computers you can control");
                if self.config.computers.is_empty() {
                    ui.label(RichText::new("None yet. Add the computer you want to control; it has to be sharing at that moment.").color(MUTED));
                }
                for (index, computer) in self.config.computers.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(RichText::new(&computer.name).strong());
                            if computer.address != computer.name {
                                ui.label(RichText::new(&computer.address).color(MUTED).small());
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.confirm_remove == Some(index) {
                                // Right-to-left: listed in reverse of how they read.
                                if ui.link("Keep").clicked() {
                                    self.confirm_remove = None;
                                }
                                if ui.link(RichText::new("Yes, remove").color(RED)).clicked() {
                                    remove = Some(index);
                                }
                            } else {
                                let button = egui::Button::new(RichText::new("Connect").color(Color32::BLACK)).fill(ACCENT);
                                if ui.add(button).clicked() {
                                    connect_to = Some(computer.clone());
                                }
                                if ui.link(RichText::new("Remove").color(MUTED)).clicked() {
                                    self.confirm_remove = Some(index);
                                }
                            }
                        });
                    });
                    ui.add_space(2.0);
                }
            });

            if action_button(ui, "Add a computer", ButtonKind::Secondary).clicked() {
                self.add = Some(AddDialog::default());
            }

            if let Some(index) = remove {
                let computer = self.config.computers.remove(index);
                // Forget the pairing too, so it really is gone; the other computer will ask for
                // a code if it is ever added again.
                if let Some(id) = auth::from_hex::<16>(&computer.id) {
                    let _ = self.paired_hosts.remove(&id);
                }
                self.confirm_remove = None;
            }
            if let Some(computer) = connect_to {
                self.target = (computer.name.clone(), computer.address.clone());
                self.pairing_code.clear();
                self.codes_sent = 0;
                self.confirm_remove = None;
                self.config.last_target = computer.clone();
                // First make sure we are paired (asking for a code here if the other computer
                // wants one); the remote desktop opens when that succeeds.
                let args = [computer.address.clone(), "--pair-only".to_owned()];
                self.client = Self::start("pyromirror-client", &args, ui.ctx(), &mut self.client_error);
            }
        }
        error_box(ui, &self.client_error);

        if self.client.is_none() && !self.viewer_running && ui.link(RichText::new("Fullscreen, mouse and sound: Client Options in Settings").small()).clicked() {
            self.config.tab = Tab::Settings;
            self.config.settings_page = SettingsPage::Connecting;
        }

        ui.add_space(4.0);
        card(ui, |ui| {
            section(ui, "While connected");
            ui.label(RichText::new("Move the pointer to the top edge of the picture for the toolbar.").color(MUTED));
            egui::Grid::new("shortcuts").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                for (keys, what) in [
                    ("Ctrl + Alt + F", "Fullscreen"),
                    ("Ctrl + Alt + G", "Send Alt+Tab, Super, ... to the remote computer"),
                    ("Ctrl + Alt + L", "Keep the mouse inside the window"),
                    ("Ctrl + Alt + M", "Relative mouse (games)"),
                    ("Ctrl + Alt + Q", "Disconnect"),
                ] {
                    ui.label(RichText::new(keys).monospace().small());
                    ui.label(RichText::new(what).color(MUTED).small());
                    ui.end_row();
                }
            });
        });
    }

    /// Colour, headline and explanation for the sharing side.
    /// The "Add a computer" dialog: address first, then (if the other computer asks) its pairing
    /// code. The computer is only saved once pairing has gone through.
    fn add_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.add else { return };
        let mut close = false;
        let mut added = None;

        // Has the pairing run finished?
        if let Some(success) = dialog.pairing.as_mut().and_then(|p| p.exited()) {
            let log = dialog.pairing.as_ref().map(|p| p.log()).unwrap_or_default();
            dialog.pairing = None;
            let host = log.iter().find_map(|l| l.text.strip_prefix("Host: ")).and_then(|rest| rest.split_once(' '));
            match (success, host) {
                (true, Some((id, name))) => {
                    added = Some(Computer { name: name.to_owned(), address: dialog.address.trim().to_owned(), id: id.to_owned() });
                }
                _ => dialog.error = Self::failure(&log),
            }
        }

        let modal = egui::Modal::new(egui::Id::new("add-computer")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(RichText::new("Add a computer").size(18.0).strong());
            ui.add_space(4.0);

            let Some(pairing) = &mut dialog.pairing else {
                // Step 1: where is it?
                ui.label(RichText::new("On the other computer, open PyroMirror and start sharing. It shows the address to enter here.").color(MUTED));
                let field = ui.add(
                    egui::TextEdit::singleline(&mut dialog.address)
                        .hint_text("Address, e.g. 192.168.1.20")
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(10, 9)),
                );
                if dialog.address.is_empty() && dialog.error.is_empty() && !field.has_focus() {
                    field.request_focus();
                }
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                error_box(ui, &dialog.error);

                let ready = !dialog.address.trim().is_empty();
                let add = ui.add_enabled_ui(ready, |ui| action_button(ui, "Add", ButtonKind::Primary).clicked()).inner;
                if ready && (add || enter) {
                    dialog.error.clear();
                    dialog.code.clear();
                    dialog.codes_sent = 0;
                    let args = [dialog.address.trim().to_owned(), "--pair-only".to_owned()];
                    dialog.pairing = Self::start("pyromirror-client", &args, ui.ctx(), &mut dialog.error);
                }
                if ui.link("Cancel").clicked() {
                    close = true;
                }
                return;
            };

            // Step 2: talking to it; it may ask for its pairing code.
            let log = pairing.log();
            let prompts = log.iter().filter(|l| l.text.starts_with("Pairing code needed")).count();
            match client_state(&log) {
                ClientState::CodeNeeded { wrong, .. } if prompts > dialog.codes_sent => {
                    let text = if wrong {
                        "That code was not right. Check the code shown on the other computer and try again."
                    } else {
                        "The other computer is now showing a pairing code. Type it here."
                    };
                    ui.label(RichText::new(text).color(if wrong { RED } else { MUTED }));
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut dialog.code)
                            .hint_text("123 456")
                            .font(egui::FontId::monospace(22.0))
                            .desired_width(f32::INFINITY)
                            .margin(Margin::symmetric(10, 9)),
                    );
                    if dialog.code.is_empty() && !field.has_focus() {
                        field.request_focus();
                    }
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let ready = dialog.code.chars().filter(|c| c.is_ascii_digit()).count() == 6;
                    let pair = ui.add_enabled_ui(ready, |ui| action_button(ui, "Pair", ButtonKind::Primary).clicked()).inner;
                    if ready && (pair || enter) {
                        pairing.send_line(dialog.code.trim());
                        dialog.codes_sent = prompts;
                        dialog.code.clear();
                    }
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Contacting {}...", dialog.address.trim()));
                    });
                }
            }
            if ui.link("Cancel").clicked() {
                close = true;
            }
        });
        // Clicking outside or pressing Escape, but not in the middle of typing a code.
        if modal.should_close() && dialog.pairing.is_none() {
            close = true;
        }

        if let Some(computer) = added {
            self.config.remember(computer);
            self.paired_hosts.reload();
            close = true;
        }
        if close {
            // Dropping the dialog ends a pairing that is still running.
            self.add = None;
        }
    }

    fn sharing_summary(&self) -> (HostState, Color32, String, String) {
        let state = match &self.demo {
            Some(demo) => demo.clone(),
            None if self.server_running => host_state(&self.server_log),
            None if self.server_error.is_empty() => HostState::Stopped,
            None => HostState::Failed,
        };
        let address = self.local_address.as_ref().map(|address| {
            if self.config.port == 9000 { address.clone() } else { format!("{}:{}", address, self.config.port) }
        });
        let (color, title, detail) = match &state {
            HostState::Stopped => (MUTED, "Sharing is off".into(), "Nobody can see or control this computer.".into()),
            HostState::Failed => (RED, "Sharing could not start".into(), "See the message below.".into()),
            HostState::Starting => (YELLOW, "Starting...".into(), String::new()),
            HostState::WaitingForPermission => (
                YELLOW,
                "Waiting for your permission".into(),
                "Your desktop is asking whether to allow sharing. Look for its dialog.".into(),
            ),
            HostState::Ready => (
                GREEN,
                "Sharing is on".into(),
                match &address {
                    Some(address) => format!("Waiting for a connection. On the other computer, connect to {}", address),
                    None => "Waiting for a connection.".into(),
                },
            ),
            HostState::PairingRequest { name, .. } => (
                YELLOW,
                format!("{} wants to connect", name),
                "Type this code on that computer to allow it. Ignore it if you were not expecting this.".into(),
            ),
            HostState::Serving(peer) => (GREEN, "Sharing is on".into(), format!("{} is connected right now.", peer)),
        };
        (state, color, title, detail)
    }

    /// "Start at login" and "share automatically". The second can only be switched on by
    /// passing a permission check, so that an unattended start never hangs on a dialog.
    fn startup_card(&mut self, ui: &mut egui::Ui, sharing: bool) {
        card(ui, |ui| {
            section(ui, "Startup");

            let mut autostart = self.config.autostart;
            if ui.checkbox(&mut autostart, "Start PyroMirror in the tray when I log in").changed() {
                self.config.autostart = autostart;
                self.apply_autostart();
            }

            if self.permission_check.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking permissions...");
                });
                ui.label(RichText::new("Your system may ask you to allow screen sharing, remote control or network access. Say yes, and choose to remember it if offered.").color(YELLOW).small());
                if ui.link("Cancel").clicked() {
                    self.permission_check = None;
                }
                return;
            }

            let mut auto_share = self.config.auto_share;
            // Needs the tray agent, and the check needs the port, so sharing must be off.
            let can_change = self.config.autostart && (auto_share || !sharing);
            let changed = ui.add_enabled(can_change, egui::Checkbox::new(&mut auto_share, "Start sharing this computer automatically at login")).changed();
            if changed && !auto_share {
                self.config.auto_share = false;
            } else if changed {
                // Not enabled yet: first prove that sharing can start without anyone answering
                // a dialog. The setting is switched on when the check passes.
                self.permission_error.clear();
                let mut args = self.config.server_args();
                args.push("--check-permissions".into());
                self.permission_check = Self::start("pyromirror-server", &args, ui.ctx(), &mut self.permission_error);
            }

            let hint = if !self.config.autostart {
                "Automatic sharing needs PyroMirror to start at login."
            } else if self.config.auto_share {
                "Permissions were checked. This computer can be reached after login without anyone at the desk."
            } else if sharing {
                "Stop sharing to switch this on: PyroMirror first checks that it has the permissions it needs."
            } else {
                "Switching this on first checks that PyroMirror has the permissions it needs."
            };
            ui.label(RichText::new(hint).color(MUTED).small());
            error_box(ui, &self.permission_error);
        });
    }

    /// Settings are grouped by the situation they apply to, because that is the question
    /// people have: "does this change what I share, or what I see?"
    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        match self.config.settings_page {
            SettingsPage::Sharing => self.sharing_settings(ui),
            SettingsPage::Connecting => self.connecting_settings(ui),
            SettingsPage::General => self.general_settings(ui),
        }
    }

    /// A line under the page tabs saying what the page governs.
    fn scope_note(ui: &mut egui::Ui, text: &str) {
        ui.label(RichText::new(text).color(MUTED));
        ui.add_space(2.0);
    }

    fn sharing_settings(&mut self, ui: &mut egui::Ui) {
        let sharing = self.server_running || self.demo.is_some();
        Self::scope_note(ui, "Host Options apply when this computer is the one being shared. They do not change what you see when you control another computer.");
        if sharing {
            ui.label(RichText::new("Sharing is on, so most of these are locked. Stop sharing to change them.").color(YELLOW).small());
        }

        // Read when sharing starts, so they cannot change underneath a session.
        ui.add_enabled_ui(!sharing, |ui| {
            card(ui, |ui| {
                section(ui, "Picture");
                row(ui, "Bitrate", |ui| {
                    ui.add(egui::Slider::new(&mut self.config.bitrate_mbps, 10..=2000).logarithmic(true).suffix(" Mbps"));
                });
                row(ui, "Frame rate", |ui| {
                    segmented(ui, &mut self.config.fps, &[(30, "30"), (60, "60"), (90, "90"), (120, "120"), (144, "144")]);
                });
                row(ui, "Resolution", |ui| {
                    segmented(ui, &mut self.config.scale, &[(1, "Native"), (2, "1/2"), (3, "1/3"), (4, "1/4")]);
                });
                row(ui, "Colour", |ui| {
                    segmented(ui, &mut self.config.chroma_444, &[(true, "4:4:4 sharp text"), (false, "4:2:0 less data")]);
                });
                ui.label(RichText::new("Higher bitrate and 4:4:4 look better and need a faster network. A lower resolution helps on Wi-Fi.").color(MUTED).small());
            });

            ui.add_space(4.0);
            card(ui, |ui| {
                section(ui, "What the other computer gets");
                ui.checkbox(&mut self.config.share_audio, "This computer's sound");
                ui.checkbox(&mut self.config.allow_control, "Control of the mouse and keyboard");
            });

            ui.add_space(4.0);
            card(ui, |ui| {
                section(ui, "Who may connect");
                ui.checkbox(&mut self.config.require_pairing, "Require pairing for new computers");
                let (hint, color) = if self.config.require_pairing {
                    ("A computer connecting for the first time has to enter a one-time code shown here.", MUTED)
                } else {
                    ("Anyone on your network can connect without a code.", YELLOW)
                };
                ui.label(RichText::new(hint).color(color).small());
            });
        });

        ui.add_space(4.0);
        ui.add_enabled_ui(!sharing, |ui| {
            card(ui, |ui| {
                section(ui, "Network");
                row(ui, "Port", |ui| {
                    ui.add(egui::DragValue::new(&mut self.config.port).range(1024..=65535).speed(1));
                });
                row(ui, "Packet size", |ui| {
                    segmented(ui, &mut self.config.mtu, &[(1400, "1400 standard"), (8900, "8900 jumbo")]);
                });
                row(ui, "Pacing", |ui| {
                    ui.add(egui::Slider::new(&mut self.config.pace_factor, 1.1..=4.0).suffix("x bitrate"));
                });
                ui.label(RichText::new("Jumbo packets need a network set up for them. Lower pacing is smoother on Wi-Fi; higher has less delay.").color(MUTED).small());
            });
        });
    }

    fn connecting_settings(&mut self, ui: &mut egui::Ui) {
        Self::scope_note(ui, "Client Options apply when you control another computer from this one. Picture quality is decided by the computer you connect to.");

        card(ui, |ui| {
            section(ui, "The remote desktop window");
            ui.checkbox(&mut self.config.fullscreen, "Start in fullscreen");
            ui.checkbox(&mut self.config.lock_mouse, "Keep the mouse inside the window");
            ui.checkbox(&mut self.config.play_audio, "Play the other computer's sound");
            ui.label(RichText::new("Used from the next connection. All three can also be switched from the toolbar while connected.").color(MUTED).small());
        });

    }

    fn general_settings(&mut self, ui: &mut egui::Ui) {
        Self::scope_note(ui, "These are about PyroMirror itself on this computer.");
        let sharing = self.server_running || self.demo.is_some();
        self.startup_card(ui, sharing);
    }

    fn host_tab(&mut self, ui: &mut egui::Ui) {
        let log = self.server_log.clone();
        let running = self.server_running || self.demo.is_some();
        let (state, color, title, detail) = self.sharing_summary();

        let address = self.local_address.clone();
        let toggle = state_panel(ui, color, &title, &detail, |ui| {
            if let (HostState::Ready, Some(address)) = (&state, &address) {
                if ui.link(RichText::new("Copy the address").color(ACCENT)).clicked() {
                    ui.ctx().copy_text(address.clone());
                }
            }
            if let HostState::PairingRequest { code, .. } = &state {
                ui.label(RichText::new(code).size(34.0).strong().monospace());
            } else if running && !self.config.require_pairing {
                ui.label(RichText::new("Pairing is off: anyone on your network can connect.").color(YELLOW));
            } else if matches!(state, HostState::Ready) {
                ui.label(RichText::new("A computer connecting for the first time will need a code that appears here.").color(MUTED).small());
            }
            if running {
                action_button(ui, "Stop sharing", ButtonKind::Stop).clicked()
            } else {
                action_button(ui, "Start sharing this computer", ButtonKind::Primary).clicked()
            }
        });
        if toggle {
            self.server_error.clear();
            if running {
                self.server.stop();
                self.sharing_expected = false;
            } else if let Err(e) = self.server.start(&self.config.server_args()) {
                self.server_error.push(LogLine { level: Level::Error, text: format!("Could not start sharing: {}", e) });
            }
            // Show the new state on the next frame rather than after the polling interval.
            self.server_polled -= std::time::Duration::from_secs(1);
        }
        error_box(ui, &self.server_error);

        paired_card(
            ui,
            "Computers allowed to connect",
            "None yet. A computer appears here once it has paired with this one: add this computer on it, and enter the code shown here.",
            "Removing one disconnects it and makes it pair again.",
            &mut self.paired_clients,
            &mut self.confirm_forget,
        );

        // What will be (or is being) shared, with the way to change it.
        ui.add_space(4.0);
        card(ui, |ui| {
            section(ui, "Sharing with these settings");
            let c = &self.config;
            let resolution = if c.scale == 1 { "native resolution".to_owned() } else { format!("1/{} resolution", c.scale) };
            ui.label(format!("{} Mbps, {} fps, {}, {}", c.bitrate_mbps, c.fps, resolution, if c.chroma_444 { "4:4:4" } else { "4:2:0" }));
            let mut extras = Vec::new();
            extras.push(if c.share_audio { "sound on" } else { "sound off" });
            extras.push(if c.allow_control { "remote control allowed" } else { "view only" });
            extras.push(if c.require_pairing { "pairing required" } else { "no pairing" });
            ui.label(RichText::new(extras.join(", ")).color(MUTED));
            if ui.link("Change in Settings").clicked() {
                self.config.tab = Tab::Settings;
                self.config.settings_page = SettingsPage::Sharing;
            }
        });

        if running && !log.is_empty() {
            ui.add_space(4.0);
            egui::CollapsingHeader::new(RichText::new("Log").color(MUTED)).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                    for line in &log {
                        log_label(ui, line);
                    }
                });
            });
        }
    }
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A list of paired computers, each with a two-step "Remove".
fn paired_card(
    ui: &mut egui::Ui,
    title: &str,
    empty: &str,
    hint: &str,
    store: &mut TokenStore,
    confirm: &mut Option<auth::Id>,
) {
    card(ui, |ui| {
        section(ui, title);
        if store.is_empty() {
            ui.label(RichText::new(empty).color(MUTED));
            return;
        }
        let mut remove = None;
        for device in store.devices() {
            ui.horizontal(|ui| {
                ui.label(&device.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if *confirm == Some(device.id) {
                        // Right-to-left: listed in reverse of how they read.
                        if ui.link("Keep").clicked() {
                            *confirm = None;
                        }
                        if ui.link(RichText::new("Yes, remove").color(RED)).clicked() {
                            remove = Some(device.id);
                        }
                    } else if ui.link(RichText::new("Remove").color(MUTED)).clicked() {
                        *confirm = Some(device.id);
                    }
                });
            });
        }
        ui.label(RichText::new(hint).color(MUTED).small());
        if let Some(id) = remove {
            if let Err(e) = store.remove(&id) {
                log::warn!("Could not update the paired computers: {}", e);
            }
            *confirm = None;
        }
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title.to_uppercase()).color(MUTED).small().strong());
}

/// A label on the left with its control on the right.
fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
    });
}

/// Mutually exclusive choices, drawn as one joined track with the chosen segment raised. It is
/// deliberately not accent-coloured: orange is reserved for buttons that do something.
fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, choices: &[(T, &str)]) {
    let font = egui::FontId::proportional(13.0);
    let enabled = ui.is_enabled();
    let widths: Vec<f32> = choices
        .iter()
        .map(|(_, label)| ui.painter().layout_no_wrap(label.to_string(), font.clone(), TEXT).size().x + 20.0)
        .collect();
    let size = Vec2::new(widths.iter().sum::<f32>() + 4.0, 28.0);
    let (track, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(track, CornerRadius::same(7), TRACK);

    let mut x = track.left() + 2.0;
    for ((choice, label), width) in choices.iter().zip(widths) {
        let rect = egui::Rect::from_min_size(egui::pos2(x, track.top() + 2.0), Vec2::new(width, track.height() - 4.0));
        x += width;
        let response = ui.interact(rect, ui.id().with(("segment", *label)), egui::Sense::click());
        if response.clicked() {
            *value = *choice;
        }
        let selected = *value == *choice;
        if selected {
            ui.painter().rect_filled(rect, CornerRadius::same(5), SEGMENT);
        }
        let color = match (selected, enabled, response.hovered()) {
            (_, false, _) => MUTED.gamma_multiply(0.7),
            (true, ..) | (_, _, true) => TEXT,
            _ => MUTED,
        };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, font.clone(), color);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ButtonKind {
    /// The one thing this screen is for: solid accent colour.
    Primary,
    /// Ends something that is running: outlined in red.
    Stop,
    /// Opens something rather than doing it: outlined in the accent colour.
    Secondary,
}

/// A full-width action button with centred text.
fn action_button(ui: &mut egui::Ui, label: &str, kind: ButtonKind) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), egui::Sense::click());
    let enabled = ui.is_enabled();
    let hovered = response.hovered() && enabled;
    let radius = CornerRadius::same(8);
    let text = match (kind, enabled) {
        (_, false) => {
            ui.painter().rect_filled(rect, radius, FIELD);
            MUTED
        }
        (ButtonKind::Primary, true) => {
            ui.painter().rect_filled(rect, radius, if hovered { ACCENT_HOVER } else { ACCENT });
            Color32::BLACK
        }
        (ButtonKind::Secondary, true) => {
            ui.painter().rect_filled(rect, radius, if hovered { FIELD_HOVER } else { FIELD });
            ui.painter().rect_stroke(rect, radius, Stroke::new(1.5, ACCENT.gamma_multiply(if hovered { 1.0 } else { 0.7 })), egui::StrokeKind::Inside);
            TEXT
        }
        (ButtonKind::Stop, true) => {
            ui.painter().rect_filled(rect, radius, RED.gamma_multiply(if hovered { 0.3 } else { 0.12 }));
            ui.painter().rect_stroke(rect, radius, Stroke::new(1.5, RED), egui::StrokeKind::Inside);
            TEXT
        }
    };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(16.0), text);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The state of one side (sharing / remote session) with the button that changes it: a tinted
/// panel with a coloured stripe, a headline and an explanation.
fn state_panel<R>(ui: &mut egui::Ui, color: Color32, title: &str, detail: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let frame = egui::Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.55)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin { left: 20, right: 14, top: 14, bottom: 14 });
    let shown = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 6.0, color);
            ui.label(RichText::new(title).size(18.0).strong());
        });
        if !detail.is_empty() {
            ui.label(RichText::new(detail).color(MUTED));
        }
        ui.add_space(6.0);
        add(ui)
    });
    // Stripe down the left edge.
    let rect = shown.response.rect;
    let stripe = egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.left() + 6.0, rect.bottom()));
    ui.painter().rect_filled(stripe, CornerRadius { nw: 10, sw: 10, ne: 0, se: 0 }, color);
    shown.inner
}

/// Navigation: plain text with an underline for the current entry, each optionally with a dot
/// that lights up while something there is active. Nothing here looks like a button. Used for
/// the main tabs and, smaller, for the pages of Settings.
fn tab_bar<T: PartialEq + Copy>(ui: &mut egui::Ui, current: &mut T, tabs: &[(T, &str, Option<Color32>)], font_size: f32, height: f32) {
    let width = ui.available_width() / tabs.len() as f32;
    let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), egui::Sense::hover());
    ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, BORDER));

    for (i, (tab, label, active)) in tabs.iter().enumerate() {
        let rect = egui::Rect::from_min_size(egui::pos2(bar.left() + width * i as f32, bar.top()), Vec2::new(width, bar.height()));
        let response = ui.interact(rect, ui.id().with(("tab", *label)), egui::Sense::click());
        if response.clicked() {
            *current = *tab;
        }
        let selected = *current == *tab;
        let color = if selected || response.hovered() { TEXT } else { MUTED };
        let font = egui::FontId::proportional(font_size);
        let text = ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, font, color);
        if let Some(dot) = active {
            ui.painter().circle_filled(egui::pos2(text.right() + 10.0, rect.center().y), 4.0, *dot);
        }
        if selected {
            let thickness = if height > 34.0 { 3.0 } else { 2.0 };
            let underline = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - thickness), rect.right_bottom());
            ui.painter().rect_filled(underline, CornerRadius::same(2), ACCENT);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
}

/// One entry of the always-visible status strip at the bottom of the window.
fn strip_item(ui: &mut egui::Ui, color: Color32, text: &str) {
    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
    ui.painter().circle_filled(dot.center(), 4.0, color);
    ui.label(RichText::new(text).color(if color == MUTED { MUTED } else { TEXT }));
}

fn log_label(ui: &mut egui::Ui, line: &LogLine) {
    let color = match line.level {
        Level::Info => MUTED,
        Level::Warn => YELLOW,
        Level::Error => RED,
    };
    ui.label(RichText::new(&line.text).color(color).small());
}

fn error_box(ui: &mut egui::Ui, lines: &[LogLine]) {
    if lines.is_empty() {
        return;
    }
    egui::Frame::new()
        .fill(RED.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, RED.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for line in lines {
                ui.label(RichText::new(&line.text).color(if line.level == Level::Info { TEXT } else { RED }).small());
            }
        });
}

impl Drop for App {
    fn drop(&mut self) {
        if self.demo.is_some() || self.screenshot.is_some() {
            return;
        }
        // With the tray agent running, sharing carries on and stays visible there. Without it,
        // nothing would show that this computer is being shared, so it stops with the window.
        if !self.agent.is_running() {
            self.server.stop();
        }
        self.window.release();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.reap();
        if self.stores_read.elapsed() >= std::time::Duration::from_secs(1) {
            self.paired_clients.reload();
            self.paired_hosts.reload();
            self.stores_read = std::time::Instant::now();
        }

        let (share_state, share_color, share_title, _) = self.sharing_summary();
        // A pairing request needs attention on the Share tab; go there once when it appears.
        let pairing = matches!(share_state, HostState::PairingRequest { .. });
        if pairing && !self.was_pairing {
            self.config.tab = Tab::Host;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        }
        self.was_pairing = pairing;
        let sharing = self.server_running || self.demo.is_some();
        let connected = self.viewer_running && client_state(&self.viewer_log) == ClientState::Connected;

        egui::Panel::top("header").frame(egui::Frame::new().fill(BG).inner_margin(Margin { left: 18, right: 18, top: 14, bottom: 0 })).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(RichText::new("Pyro").size(24.0).strong().color(ACCENT));
                ui.label(RichText::new("Mirror").size(24.0).strong());
            });
            ui.add_space(4.0);
            tab_bar(
                ui,
                &mut self.config.tab,
                &[
                    (Tab::Connect, "Connect", connected.then_some(GREEN)),
                    (Tab::Host, "Share", sharing.then_some(share_color)),
                    (Tab::Settings, "Settings", None),
                ],
                15.0,
                40.0,
            );
            if self.config.tab == Tab::Settings {
                tab_bar(
                    ui,
                    &mut self.config.settings_page,
                    &[
                        (SettingsPage::Sharing, "Host Options", None),
                        (SettingsPage::Connecting, "Client Options", None),
                        (SettingsPage::General, "General", None),
                    ],
                    13.0,
                    32.0,
                );
            }
        });

        // Always visible, whichever tab is open: what is running right now.
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(TRACK).inner_margin(Margin::symmetric(18, 8))).show(ui, |ui| {
            ui.horizontal(|ui| {
                strip_item(ui, if sharing { share_color } else { MUTED }, &format!("This computer: {}", share_title.to_lowercase()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let text = if connected { format!("Remote session: {}", self.target.0) } else { "Remote session: none".into() };
                    // Right-to-left: the label goes in first, then its dot.
                    ui.label(RichText::new(text).color(if connected { TEXT } else { MUTED }));
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, if connected { GREEN } else { MUTED });
                });
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(Margin { left: 18, right: 18, top: 14, bottom: 14 })).show(ui, |ui| {
            // Screenshot mode can ask for the bottom of the page ("host-bottom").
            let bottom = self.scroll_to_bottom;
            egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(bottom).show(ui, |ui| match self.config.tab {
                Tab::Connect => self.connect_tab(ui),
                Tab::Host => self.host_tab(ui),
                Tab::Settings => self.settings_tab(ui),
            });
        });

        self.add_dialog(ui.ctx());

        if self.config != self.saved {
            self.config.save();
            self.saved = self.config.clone();
        }

        // The server and the tray agent change things behind our back; keep looking.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));

        if let Some(path) = &self.screenshot {
            self.frames += 1;
            let ctx = ui.ctx();
            if self.frames == 5 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            let image = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = image {
                let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
                ppm.extend(image.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]));
                let _ = std::fs::write(path, ppm);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(texts: &[&str]) -> Vec<LogLine> {
        texts.iter().map(|t| LogLine { level: Level::Info, text: t.to_string() }).collect()
    }

    #[test]
    fn client_state_follows_the_viewer_log() {
        let mut log = lines(&["Connecting to 10.0.0.2:9000"]);
        assert!(client_state(&log) == ClientState::Connecting);
        log.extend(lines(&["Pairing code needed: enter the code shown on the host"]));
        assert!(client_state(&log) == ClientState::CodeNeeded { wrong: false, revoked: false });
        log.extend(lines(&["Wrong pairing code, try again", "Pairing code needed: enter the code shown on the host"]));
        assert!(client_state(&log) == ClientState::CodeNeeded { wrong: true, revoked: false });

        let revoked = lines(&[
            "Connecting to 10.0.0.2:9000",
            "This computer is no longer paired with the host; it has to be paired again",
            "Pairing code needed: enter the code shown on the host",
        ]);
        assert!(client_state(&revoked) == ClientState::CodeNeeded { wrong: false, revoked: true });
        log.extend(lines(&["Paired with this host; no code will be needed next time", "Stream: 1920x1080 @ 60 fps"]));
        assert!(client_state(&log) == ClientState::Connected);
    }

    #[test]
    fn host_state_follows_the_server_log() {
        assert!(host_state(&[]) == HostState::Starting);
        let mut log = lines(&["Waiting for remote control permission (check for a dialog from your desktop)..."]);
        assert!(host_state(&log) == HostState::WaitingForPermission);
        log.extend(lines(&["Listening on 0.0.0.0:9000 (TCP control + UDP video); waiting for a client"]));
        assert!(host_state(&log) == HostState::Ready);
        // A connection alone is not "serving": the client has to be accepted first.
        log.extend(lines(&["Client connected from 192.168.1.7:51234"]));
        assert!(host_state(&log) == HostState::Ready);
        log.extend(lines(&["Pairing request from caio: laptop: code 042917"]));
        assert!(host_state(&log) == HostState::PairingRequest { name: "caio: laptop".into(), code: "042 917".into() });
        log.extend(lines(&["Accepted caio: laptop", "Sending video to 192.168.1.7:40000"]));
        assert!(host_state(&log) == HostState::Serving("caio: laptop (192.168.1.7)".into()));
        log.extend(lines(&["Client 192.168.1.7:51234 disconnected"]));
        assert!(host_state(&log) == HostState::Ready);

        // A computer that only sets up or checks its pairing never counts as connected.
        log.extend(lines(&["Client connected from 192.168.1.7:51300", "caio: laptop checked its pairing"]));
        assert!(host_state(&log) == HostState::Ready);
        log.extend(lines(&["Client 192.168.1.7:51300 disconnected"]));
        assert!(host_state(&log) == HostState::Ready);
    }
}
