//! PyroMirror launcher: one window to connect to a host or to share this computer.
//!
//! The streaming itself is done by `pyromirror-client` and `pyromirror-server`, which this
//! program starts with the chosen settings and watches. Everything is drawn by egui, so the
//! window looks the same on every platform.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod process;

use std::path::PathBuf;

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};

use config::{Config, Tab};
use process::{Level, LogLine, Process};

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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PyroMirror")
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

/// What the server is doing, as far as its log tells.
#[derive(Clone, PartialEq)]
enum HostState {
    Stopped,
    Starting,
    WaitingForPermission,
    Ready,
    Serving(String),
    Failed,
}

fn host_state(log: &[LogLine]) -> HostState {
    let mut state = HostState::Starting;
    for line in log {
        let text = line.text.as_str();
        if text.starts_with("Waiting for") && text.contains("permission") {
            state = HostState::WaitingForPermission;
        } else if text.starts_with("Listening on") {
            state = HostState::Ready;
        } else if let Some(peer) = text.strip_prefix("Client connected from ") {
            // Drop the ephemeral TCP port; the address is what people recognise.
            let host = peer.rsplit_once(':').map_or(peer, |(host, _)| host);
            state = HostState::Serving(host.to_owned());
        } else if text.ends_with(" disconnected") || text.starts_with("Session with ") {
            state = HostState::Ready;
        }
    }
    state
}

/// The pairing code the server announced, formatted for reading aloud ("123 456").
fn pairing_code(log: &[LogLine]) -> Option<String> {
    let code = log.iter().find_map(|line| line.text.strip_prefix("Pairing code: "))?;
    Some(if code.len() == 6 { format!("{} {}", &code[..3], &code[3..]) } else { code.to_owned() })
}

/// This computer's address on the network it would use to reach others. No packet is sent:
/// connecting a UDP socket only makes the OS pick a route.
fn local_address() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
}

struct App {
    config: Config,
    saved: Config,
    server: Option<Process>,
    client: Option<Process>,
    /// Last lines of a process that stopped on its own with an error.
    server_error: Vec<LogLine>,
    client_error: Vec<LogLine>,
    local_address: Option<String>,
    /// Pairing code typed on the Connect tab; deliberately not part of the saved settings.
    pairing_code: String,
    screenshot: Option<PathBuf>,
    /// Screenshot mode only: pretend the server is in this state.
    demo: Option<HostState>,
    frames: u32,
}

impl App {
    fn new(screenshot: Option<(PathBuf, Option<String>)>) -> Self {
        let mut config = Config::load();
        let saved = config.clone();
        let mut demo = None;
        let screenshot = screenshot.map(|(path, tab)| {
            config.tab = if tab.as_deref().is_some_and(|t| t.starts_with("host")) { Tab::Host } else { Tab::Connect };
            demo = match tab.as_deref() {
                Some("host-ready") => Some(HostState::Ready),
                Some("host-serving") => Some(HostState::Serving("192.168.1.7".into())),
                _ => None,
            };
            path
        });
        Self {
            config,
            saved,
            server: None,
            client: None,
            server_error: Vec::new(),
            client_error: Vec::new(),
            local_address: local_address(),
            pairing_code: String::new(),
            screenshot,
            demo,
            frames: 0,
        }
    }

    /// Notices processes that ended by themselves and keeps what they said if they failed.
    fn reap(&mut self) {
        for (process, error) in [(&mut self.server, &mut self.server_error), (&mut self.client, &mut self.client_error)] {
            if let Some(success) = process.as_mut().and_then(|p| p.exited()) {
                if !success {
                    let log = process.as_ref().map(|p| p.log()).unwrap_or_default();
                    *error = log[log.len().saturating_sub(6)..].to_vec();
                    if error.is_empty() {
                        error.push(LogLine { level: Level::Error, text: "The program exited unexpectedly.".into() });
                    }
                }
                *process = None;
            }
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
        if self.client.is_some() {
            let title = format!("Connected to {}", self.config.address.trim());
            let stop = state_panel(ui, GREEN, &title, "The remote desktop is open in its own window.", |ui| {
                action_button(ui, "Disconnect", ButtonKind::Stop).clicked()
            });
            if stop {
                self.client = None;
            }
        } else {
            let (color, title) = if self.client_error.is_empty() {
                (MUTED, "Not connected")
            } else {
                (RED, "The connection ended with an error")
            };
            let connect = state_panel(ui, color, title, "Enter the address shown on the computer you want to control.", |ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.config.address)
                        .hint_text("Address, e.g. 192.168.1.20")
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(10, 9)),
                );
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                if !self.config.recent.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Recent:").color(MUTED).small());
                        for address in self.config.recent.clone() {
                            if ui.link(RichText::new(&address).small()).clicked() {
                                self.config.address = address;
                            }
                        }
                    });
                }

                ui.horizontal(|ui| {
                    ui.label(RichText::new("Pairing code").color(MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pairing_code)
                            .hint_text("only the first time")
                            .desired_width(150.0)
                            .margin(Margin::symmetric(10, 6)),
                    );
                });

                let ready = !self.config.address.trim().is_empty();
                let clicked = ui.add_enabled_ui(ready, |ui| action_button(ui, "Connect", ButtonKind::Primary).clicked()).inner;
                ready && (clicked || enter)
            });
            if connect {
                let address = self.config.address.trim().to_owned();
                self.config.remember(&address);
                self.client = Self::start("pyromirror-client", &self.config.client_args(&self.pairing_code), ui.ctx(), &mut self.client_error);
            }
        }
        error_box(ui, &self.client_error);

        ui.add_space(4.0);
        ui.add_enabled_ui(self.client.is_none(), |ui| {
            card(ui, |ui| {
                section(ui, "Options");
                ui.checkbox(&mut self.config.fullscreen, "Start in fullscreen");
                ui.checkbox(&mut self.config.lock_mouse, "Keep the mouse inside the window");
                ui.checkbox(&mut self.config.play_audio, "Play the remote computer's sound");
            });
        });

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
    fn sharing_summary(&self) -> (HostState, Color32, String, String) {
        let state = match &self.server {
            None if self.demo.is_some() => self.demo.clone().unwrap(),
            None if self.server_error.is_empty() => HostState::Stopped,
            None => HostState::Failed,
            Some(server) => host_state(&server.log()),
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
            HostState::Serving(peer) => (GREEN, "Sharing is on".into(), format!("{} is connected right now.", peer)),
        };
        (state, color, title, detail)
    }

    fn host_tab(&mut self, ui: &mut egui::Ui) {
        let log = self.server.as_ref().map(|p| p.log()).unwrap_or_default();
        let running = self.server.is_some() || self.demo.is_some();
        let (state, color, title, detail) = self.sharing_summary();

        let address = self.local_address.clone();
        let toggle = state_panel(ui, color, &title, &detail, |ui| {
            if let (HostState::Ready, Some(address)) = (&state, &address) {
                if ui.link(RichText::new("Copy the address").color(ACCENT)).clicked() {
                    ui.ctx().copy_text(address.clone());
                }
            }
            let code = if self.demo.is_some() { Some("482 913".to_owned()) } else { pairing_code(&log) };
            if let Some(code) = code {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Pairing code").color(MUTED));
                    ui.label(RichText::new(code).size(22.0).strong().monospace());
                });
                ui.label(RichText::new("A computer connecting for the first time has to enter this code.").color(MUTED).small());
            } else if running && !self.config.require_pairing {
                ui.label(RichText::new("Pairing is off: anyone on your network can connect.").color(YELLOW));
            }
            if running {
                action_button(ui, "Stop sharing", ButtonKind::Stop).clicked()
            } else {
                action_button(ui, "Start sharing this computer", ButtonKind::Primary).clicked()
            }
        });
        if toggle {
            if running {
                self.server = None;
            } else {
                self.server = Self::start("pyromirror-server", &self.config.server_args(), ui.ctx(), &mut self.server_error);
            }
        }
        error_box(ui, &self.server_error);

        ui.add_space(4.0);
        ui.add_enabled_ui(!running, |ui| {
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
            });

            ui.add_space(4.0);
            card(ui, |ui| {
                section(ui, "Sharing");
                ui.checkbox(&mut self.config.share_audio, "Share this computer's sound");
                ui.checkbox(&mut self.config.allow_control, "Allow mouse and keyboard control");
                ui.checkbox(&mut self.config.require_pairing, "Ask new computers for a pairing code");
                egui::CollapsingHeader::new(RichText::new("Network").color(MUTED)).show(ui, |ui| {
                    row(ui, "Port", |ui| {
                        ui.add(egui::DragValue::new(&mut self.config.port).range(1024..=65535).speed(1));
                    });
                    row(ui, "Packet size", |ui| {
                        segmented(ui, &mut self.config.mtu, &[(1400, "1400 standard"), (8900, "8900 jumbo")]);
                    });
                    row(ui, "Pacing", |ui| {
                        ui.add(egui::Slider::new(&mut self.config.pace_factor, 1.1..=4.0).suffix("x bitrate"))
                            .on_hover_text("How fast packets leave. Lower is smoother for Wi-Fi, higher has less delay.");
                    });
                });
            });
        });
        if running {
            ui.label(RichText::new("Settings are locked while sharing is on. Stop sharing to change them.").color(MUTED).small());
        }

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

/// Navigation tabs: plain text with an underline for the current one, each with a dot that
/// lights up while that side is active. Nothing here looks like a button.
fn tab_bar(ui: &mut egui::Ui, current: &mut Tab, tabs: &[(Tab, &str, Option<Color32>)]) {
    let width = ui.available_width() / tabs.len() as f32;
    let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), egui::Sense::hover());
    ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, BORDER));

    for (i, (tab, label, active)) in tabs.iter().enumerate() {
        let rect = egui::Rect::from_min_size(egui::pos2(bar.left() + width * i as f32, bar.top()), Vec2::new(width, bar.height()));
        let response = ui.interact(rect, ui.id().with(("tab", i)), egui::Sense::click());
        if response.clicked() {
            *current = *tab;
        }
        let selected = *current == *tab;
        let color = if selected || response.hovered() { TEXT } else { MUTED };
        let font = egui::FontId::proportional(15.0);
        let text = ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, font, color);
        if let Some(dot) = active {
            ui.painter().circle_filled(egui::pos2(text.right() + 10.0, rect.center().y), 4.0, *dot);
        }
        if selected {
            let underline = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 3.0), rect.right_bottom());
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.reap();

        let (_, share_color, share_title, _) = self.sharing_summary();
        let sharing = self.server.is_some() || self.demo.is_some();
        let connected = self.client.is_some();

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
                    (Tab::Connect, "Control another computer", connected.then_some(GREEN)),
                    (Tab::Host, "Share this computer", sharing.then_some(share_color)),
                ],
            );
        });

        // Always visible, whichever tab is open: what is running right now.
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(TRACK).inner_margin(Margin::symmetric(18, 8))).show(ui, |ui| {
            ui.horizontal(|ui| {
                strip_item(ui, if sharing { share_color } else { MUTED }, &format!("This computer: {}", share_title.to_lowercase()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let text = if connected { format!("Remote session: {}", self.config.address.trim()) } else { "Remote session: none".into() };
                    // Right-to-left: the label goes in first, then its dot.
                    ui.label(RichText::new(text).color(if connected { TEXT } else { MUTED }));
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, if connected { GREEN } else { MUTED });
                });
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(Margin { left: 18, right: 18, top: 14, bottom: 14 })).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.config.tab {
                Tab::Connect => self.connect_tab(ui),
                Tab::Host => self.host_tab(ui),
            });
        });

        if self.config != self.saved {
            self.config.save();
            self.saved = self.config.clone();
        }

        // While something is running, poll for it ending even if it logs nothing.
        if self.server.is_some() || self.client.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }

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
    fn pairing_code_is_read_from_the_log() {
        assert_eq!(pairing_code(&lines(&["Capturing audio at 48000 Hz", "Pairing code: 042917"])), Some("042 917".into()));
        assert_eq!(pairing_code(&lines(&["Pairing is off: anyone who can reach this computer can connect"])), None);
    }

    #[test]
    fn host_state_follows_the_server_log() {
        assert!(host_state(&[]) == HostState::Starting);
        let mut log = lines(&["Waiting for remote control permission (check for a dialog from your desktop)..."]);
        assert!(host_state(&log) == HostState::WaitingForPermission);
        log.extend(lines(&["Listening on 0.0.0.0:9000 (TCP control + UDP video); waiting for a client"]));
        assert!(host_state(&log) == HostState::Ready);
        log.extend(lines(&["Client connected from 192.168.1.7:51234", "Sending video to 192.168.1.7:40000"]));
        assert!(host_state(&log) == HostState::Serving("192.168.1.7".into()));
        log.extend(lines(&["Client 192.168.1.7:51234 disconnected"]));
        assert!(host_state(&log) == HostState::Ready);
    }
}
