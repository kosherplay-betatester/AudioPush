//! The egui front end.
//!
//! The GUI never does any audio or network work itself. It reads atomics from
//! `Shared`, and starts or stops worker threads through `Session`. Device
//! enumeration is the one blocking call, and it runs on a short-lived thread
//! so that COM apartments never collide with winit.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;
use wasapi::Direction;

use crate::audio::devices::{self, DeviceInfo};
use crate::config::{AUDIO_PORT, BLOCK_MS, Latency, PEER_TIMEOUT_MS, STREAM_TIMEOUT_MS};
use crate::engine::{MicConfig, Session};
use crate::net::discovery::{self, PairingQueue, PeerList};
use crate::net::pair::{PairingHost, pair_with};
use crate::net::scan::{self, ScanProgress};
use crate::net::{LocalAddress, hostname, local_addresses};
use crate::pairing::{Pin, Token, TokenStore};
use crate::settings::{Pairing, Recent, Settings};
use crate::state::{Peer, Shared, now_ms};
use crate::tray::Tray;

/// Which screen the user is on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Chooser,
    Sender,
    Receiver,
}

/// Background discovery listener, alive only while the sender screen is open.
struct Discovery {
    socket: Arc<UdpSocket>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub struct AudioPushApp {
    screen: Screen,
    preset: Latency,
    shared: Arc<Shared>,
    session: Option<Session>,

    // Sender-side state
    output_devices: Vec<DeviceInfo>,
    input_devices: Vec<DeviceInfo>,
    capture_device: Option<String>,
    peers: PeerList,
    discovery: Option<Discovery>,
    discovery_error: Option<String>,
    scan_progress: Arc<ScanProgress>,
    manual_ip: String,
    manual_error: Option<String>,
    target: Option<SocketAddr>,
    target_label: String,

    // Receiver-side state
    playback_device: Option<String>,
    local_ips: Vec<LocalAddress>,

    // Return microphone stream
    mic_enabled: bool,
    mic_input_device: Option<String>,
    mic_output_device: Option<String>,
    /// Counters and level for the microphone stream, kept apart from the
    /// main one so one stream failing cannot blank the other's readout.
    mic_shared: Arc<Shared>,

    // Pairing
    /// Issues this machine's PIN while it is receiving.
    pairing_host: Arc<PairingHost>,
    /// Senders that have just paired, waiting to be written to settings.
    pairing_queue: PairingQueue,
    /// What the user is typing when pairing with a receiver.
    pin_input: String,
    /// Result of a pairing attempt, filled in by its worker thread.
    pairing_result: Arc<Mutex<Option<Result<(Token, String), String>>>>,
    pairing_busy: bool,
    pairing_error: Option<String>,

    // Remembered between runs
    settings: Settings,

    // Shell
    tray: Option<Tray>,
    hidden: bool,
    device_refresh: Instant,
    address_refresh: Instant,
}

impl AudioPushApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> AudioPushApp {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        // Pick up what was chosen last time, so a repeat run is one click.
        let settings = Settings::load();
        let target = settings.most_recent().map(|r| r.addr);
        let target_label = settings
            .most_recent()
            .map(|r| {
                if r.name.is_empty() {
                    r.addr.to_string()
                } else {
                    format!("{} ({})", r.name, r.addr.ip())
                }
            })
            .unwrap_or_default();

        AudioPushApp {
            screen: Screen::Chooser,
            preset: settings.preset.unwrap_or(Latency::Balanced),
            shared: Arc::new(Shared::new()),
            session: None,
            output_devices: list_devices(Direction::Render),
            input_devices: list_devices(Direction::Capture),
            capture_device: settings.capture_device.clone(),
            peers: discovery::new_peer_list(),
            discovery: None,
            discovery_error: None,
            scan_progress: Arc::new(ScanProgress::default()),
            manual_ip: target.map(|a| a.ip().to_string()).unwrap_or_default(),
            manual_error: None,
            target,
            target_label,
            playback_device: settings.playback_device.clone(),
            local_ips: local_addresses(),
            mic_enabled: settings.mic_enabled,
            mic_input_device: settings.mic_input_device.clone(),
            mic_output_device: settings.mic_output_device.clone(),
            mic_shared: Arc::new(Shared::new()),
            pairing_host: Arc::new(PairingHost::new()),
            pairing_queue: discovery::new_pairing_queue(),
            pin_input: String::new(),
            pairing_result: Arc::new(Mutex::new(None)),
            pairing_busy: false,
            pairing_error: None,
            settings,
            tray: None,
            hidden: false,
            device_refresh: Instant::now(),
            address_refresh: Instant::now(),
        }
    }

    fn streaming(&self) -> bool {
        self.session.is_some()
    }

    fn stop_session(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.shutdown();
        }
        self.shared.set_peer(None);
        self.shared.set_audio_error(None);
        self.shared.set_net_error(None);
        self.shared.set_level(0.0);
    }

    fn go_to(&mut self, screen: Screen) {
        if self.screen == screen {
            return;
        }
        self.stop_session();
        // Discovery only belongs to the sender screen; dropping it frees the
        // discovery port so this PC could be used as a receiver instead.
        self.discovery = None;
        self.discovery_error = None;
        self.screen = screen;
        self.refresh_devices();
        if screen == Screen::Sender {
            self.start_discovery();
        }
    }

    fn start_discovery(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        match discovery::start_listener(Arc::clone(&self.peers), Arc::clone(&stop)) {
            Ok((socket, handle)) => {
                self.discovery = Some(Discovery {
                    socket,
                    stop,
                    handle: Some(handle),
                });
                self.discovery_error = None;
            }
            Err(err) => {
                self.discovery_error = Some(err);
            }
        }
    }

    fn refresh_devices(&mut self) {
        self.output_devices = list_devices(Direction::Render);
        self.input_devices = list_devices(Direction::Capture);
        self.device_refresh = Instant::now();
    }

    /// Writes the preset and device choices out when they change.
    ///
    /// Called once a frame; the comparison is trivial and the file is only
    /// touched on an actual change, so this costs nothing while idle.
    fn sync_settings(&mut self) {
        let mut dirty = false;
        if self.settings.preset != Some(self.preset) {
            self.settings.preset = Some(self.preset);
            dirty = true;
        }
        if self.settings.capture_device != self.capture_device {
            self.settings.capture_device = self.capture_device.clone();
            dirty = true;
        }
        if self.settings.playback_device != self.playback_device {
            self.settings.playback_device = self.playback_device.clone();
            dirty = true;
        }
        if self.settings.mic_enabled != self.mic_enabled {
            self.settings.mic_enabled = self.mic_enabled;
            dirty = true;
        }
        if self.settings.mic_input_device != self.mic_input_device {
            self.settings.mic_input_device = self.mic_input_device.clone();
            dirty = true;
        }
        if self.settings.mic_output_device != self.mic_output_device {
            self.settings.mic_output_device = self.mic_output_device.clone();
            dirty = true;
        }
        if dirty {
            self.settings.save();
        }
    }

    /// Records the chosen receiver so the next run starts with it selected.
    fn remember_target(&mut self, addr: SocketAddr, name: &str) {
        self.target = Some(addr);
        self.target_label = if name.is_empty() {
            addr.to_string()
        } else {
            format!("{name} ({})", addr.ip())
        };
        self.manual_error = None;
        self.settings.push_recent(Recent {
            addr,
            name: name.to_string(),
        });
        self.settings.save();
    }

    /// Empties the discovered list and forgets the saved receiver.
    ///
    /// Receivers that are still running reappear within a second, which is
    /// the point: this clears what is stale, not what is live.
    fn clear_list(&mut self) {
        if let Ok(mut peers) = self.peers.lock() {
            peers.clear();
        }
        self.target = None;
        self.target_label.clear();
        self.manual_ip.clear();
        self.manual_error = None;
        self.settings.clear_recent();
    }

    fn live_peers(&self) -> Vec<Peer> {
        let Ok(list) = self.peers.lock() else {
            return Vec::new();
        };
        let now = now_ms();
        list.iter()
            .filter(|p| now.saturating_sub(p.last_seen_ms) < PEER_TIMEOUT_MS)
            .cloned()
            .collect()
    }
}

impl eframe::App for AudioPushApp {
    /// Runs before every repaint, and also while the window is hidden in the
    /// tray — which is exactly when the tray menu needs to be serviced.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep meters and peer lists moving without burning a core.
        ctx.request_repaint_after(Duration::from_millis(80));

        self.handle_tray(ctx);
        self.drain_new_pairings();

        // Cheap periodic refreshes so hot-plugged hardware shows up.
        if self.device_refresh.elapsed() > Duration::from_secs(5) && !self.streaming() {
            self.refresh_devices();
        }
        if self.address_refresh.elapsed() > Duration::from_secs(5) {
            self.local_ips = local_addresses();
            self.address_refresh = Instant::now();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.screen {
                Screen::Chooser => self.chooser_ui(ui),
                Screen::Sender => self.sender_ui(ui),
                Screen::Receiver => self.receiver_ui(ui),
            });
        });
        self.sync_settings();
    }

    fn on_exit(&mut self) {
        self.stop_session();
        self.discovery = None;
    }
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

impl AudioPushApp {
    fn chooser_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.heading("AudioPush");
            ui.label(
                egui::RichText::new("Low-latency system audio over your local network")
                    .color(egui::Color32::GRAY),
            );
        });
        ui.add_space(28.0);

        let width = ui.available_width().min(420.0);
        ui.vertical_centered(|ui| {
            if ui
                .add_sized(
                    [width, 72.0],
                    egui::Button::new(
                        egui::RichText::new("SEND  from this PC").size(19.0).strong(),
                    ),
                )
                .on_hover_text("Capture whatever this PC is playing and stream it out")
                .clicked()
            {
                self.go_to(Screen::Sender);
            }
            ui.label(
                egui::RichText::new("The workstation with no speakers")
                    .color(egui::Color32::GRAY)
                    .small(),
            );

            ui.add_space(18.0);

            if ui
                .add_sized(
                    [width, 72.0],
                    egui::Button::new(
                        egui::RichText::new("RECEIVE  on this PC").size(19.0).strong(),
                    ),
                )
                .on_hover_text("Play audio streamed from another PC through this PC's speakers")
                .clicked()
            {
                self.go_to(Screen::Receiver);
            }
            ui.label(
                egui::RichText::new("The laptop with the speakers")
                    .color(egui::Color32::GRAY)
                    .small(),
            );
        });

        ui.add_space(26.0);
        ui.separator();
        ui.label(
            egui::RichText::new(format!(
                "This PC is {}. Start the receiver first, then pick it on the sender.",
                hostname()
            ))
            .color(egui::Color32::GRAY)
            .small(),
        );
    }

    fn sender_ui(&mut self, ui: &mut egui::Ui) {
        self.header(ui, "Sender");

        // --- capture device -------------------------------------------------
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Capture from").strong());
        let selected_name = self
            .capture_device
            .as_ref()
            .and_then(|id| {
                self.output_devices
                    .iter()
                    .find(|d| &d.id == id)
                    .map(|d| d.name.clone())
            })
            .unwrap_or_else(|| "Default output (follow Windows)".to_string());

        let streaming = self.streaming();
        ui.add_enabled_ui(!streaming, |ui| {
            egui::ComboBox::from_id_salt("capture-device")
                .width(ui.available_width() - 8.0)
                .selected_text(selected_name)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.capture_device,
                        None,
                        "Default output (follow Windows)",
                    );
                    for device in &self.output_devices {
                        ui.selectable_value(
                            &mut self.capture_device,
                            Some(device.id.clone()),
                            &device.name,
                        );
                    }
                });
        });
        ui.label(
            egui::RichText::new(
                "This is the output Windows plays to; AudioPush taps it with WASAPI loopback.",
            )
            .color(egui::Color32::GRAY)
            .small(),
        );

        // --- receivers ------------------------------------------------------
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Receivers on this network").strong());

        if let Some(err) = &self.discovery_error {
            ui.colored_label(egui::Color32::from_rgb(235, 150, 60), err);
        }

        let peers = self.live_peers();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_height(74.0);
            ui.set_width(ui.available_width());
            if peers.is_empty() && self.settings.recent.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("Looking for receivers...")
                            .color(egui::Color32::GRAY),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Open AudioPush on the other PC, choose RECEIVE and press START.",
                        )
                        .color(egui::Color32::DARK_GRAY)
                        .small(),
                    );
                });
            } else {
                for peer in &peers {
                    let selected = self.target == Some(peer.addr);
                    let label = format!("{}  —  {}", peer.hostname, peer.addr.ip());
                    if ui.selectable_label(selected, label).clicked() && !streaming {
                        let (addr, name) = (peer.addr, peer.hostname.clone());
                        self.remember_target(addr, &name);
                    }
                }
            }

            // Receivers used before, when they are not announcing right
            // now. Offered anyway: usually one is simply not started yet,
            // and streaming to it will work the moment it is.
            let saved: Vec<Recent> = self
                .settings
                .recent
                .iter()
                .filter(|entry| !peers.iter().any(|p| p.addr == entry.addr))
                .cloned()
                .collect();
            for entry in saved {
                let selected = self.target == Some(entry.addr);
                let label = format!("{}   (recent)", entry.label());
                if ui
                    .selectable_label(selected, egui::RichText::new(label).weak())
                    .on_hover_text("Used before; not announcing right now")
                    .clicked()
                    && !streaming
                {
                    self.remember_target(entry.addr, &entry.name);
                }
            }
        });

        // --- fallbacks ------------------------------------------------------
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let scanning = self.scan_progress.running.load(Ordering::Relaxed);
            let can_scan = self.discovery.is_some() && !scanning;
            if ui
                .add_enabled(can_scan, egui::Button::new("Scan network"))
                .on_hover_text(
                    "Asks every address on your subnet whether AudioPush is listening there",
                )
                .clicked()
            {
                if let Some(discovery) = &self.discovery {
                    scan::spawn(
                        Arc::clone(&discovery.socket),
                        Arc::clone(&self.scan_progress),
                    );
                }
            }
            if ui
                .add_enabled(!streaming, egui::Button::new("Clear list"))
                .on_hover_text(
                    "Empties the list and forgets the saved address. \
                     Receivers that are running reappear within a second.",
                )
                .clicked()
            {
                self.clear_list();
            }

            if scanning {
                let sent = self.scan_progress.sent.load(Ordering::Relaxed);
                let total = self.scan_progress.total.load(Ordering::Relaxed).max(1);
                ui.add(
                    egui::ProgressBar::new(sent as f32 / total as f32)
                        .desired_width(120.0)
                        .text(format!("{sent}/{total}")),
                );
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Or type an address:");
            ui.add_enabled(
                !streaming,
                egui::TextEdit::singleline(&mut self.manual_ip)
                    .hint_text("192.168.1.20")
                    .desired_width(150.0),
            );
            if ui.add_enabled(!streaming, egui::Button::new("Use")).clicked() {
                match parse_target(&self.manual_ip) {
                    Ok(addr) => self.remember_target(addr, ""),
                    Err(err) => self.manual_error = Some(err),
                }
            }
        });
        if let Some(err) = &self.manual_error {
            ui.colored_label(egui::Color32::from_rgb(235, 120, 100), err);
        }

        self.pairing_ui(ui);
        self.mic_ui(ui, true);
        self.latency_ui(ui);
        self.start_stop_ui(ui, true);
        self.status_ui(ui, true);
    }

    fn receiver_ui(&mut self, ui: &mut egui::Ui) {
        self.header(ui, "Receiver");

        ui.add_space(6.0);
        ui.label(egui::RichText::new("Play through").strong());
        let selected_name = self
            .playback_device
            .as_ref()
            .and_then(|id| {
                self.output_devices
                    .iter()
                    .find(|d| &d.id == id)
                    .map(|d| d.name.clone())
            })
            .unwrap_or_else(|| "Default output (follow Windows)".to_string());

        let streaming = self.streaming();
        ui.add_enabled_ui(!streaming, |ui| {
            egui::ComboBox::from_id_salt("playback-device")
                .width(ui.available_width() - 8.0)
                .selected_text(selected_name)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.playback_device,
                        None,
                        "Default output (follow Windows)",
                    );
                    for device in &self.output_devices {
                        ui.selectable_value(
                            &mut self.playback_device,
                            Some(device.id.clone()),
                            &device.name,
                        );
                    }
                });
        });

        // --- how to reach this machine --------------------------------------
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Tell the sender to connect to").strong());
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.local_ips.is_empty() {
                ui.colored_label(
                    egui::Color32::from_rgb(235, 150, 60),
                    "No network connection found.",
                );
            } else {
                for local in &self.local_ips {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{}:{}", local.ip, AUDIO_PORT))
                                .monospace()
                                .size(15.0),
                        );
                        ui.label(
                            egui::RichText::new(&local.interface)
                                .color(egui::Color32::GRAY)
                                .small(),
                        );
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(local.ip.to_string());
                        }
                    });
                }
            }
            ui.label(
                egui::RichText::new(format!(
                    "This PC announces itself as \"{}\" — usually the sender just finds it.",
                    hostname()
                ))
                .color(egui::Color32::GRAY)
                .small(),
            );
        });

        self.pin_ui(ui);
        self.mic_ui(ui, false);
        self.latency_ui(ui);
        self.start_stop_ui(ui, false);
        self.status_ui(ui, false);
    }
}

// ---------------------------------------------------------------------------
// Shared UI pieces
// ---------------------------------------------------------------------------

impl AudioPushApp {
    fn header(&mut self, ui: &mut egui::Ui, title: &str) {
        ui.horizontal(|ui| {
            if ui.button("< Back").clicked() {
                self.go_to(Screen::Chooser);
            }
            ui.heading(title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.tray.is_some() {
                    if ui.button("Hide to tray").clicked() {
                        self.hidden = true;
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    }
                } else if ui
                    .button("Hide to tray")
                    .on_hover_text("Puts AudioPush in the notification area; it keeps streaming")
                    .clicked()
                {
                    self.tray = Tray::create(ui.ctx().clone());
                    if self.tray.is_some() {
                        self.hidden = true;
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    }
                }
            });
        });
        ui.separator();
    }

    /// Sender side: prove we know the receiver's PIN, and keep the token.
    ///
    /// Shown only while the chosen receiver is unpaired. Once paired, the
    /// token is stored and this never appears for that machine again.
    fn pairing_ui(&mut self, ui: &mut egui::Ui) {
        let Some(target) = self.target else { return };
        if self.settings.token_for(target.ip()).is_some() {
            return;
        }

        // Collect the result of a pairing attempt running on its own thread.
        if self.pairing_busy {
            let finished = self
                .pairing_result
                .lock()
                .ok()
                .and_then(|mut slot| slot.take());
            if let Some(outcome) = finished {
                self.pairing_busy = false;
                match outcome {
                    Ok((token, name)) => {
                        self.settings.add_pairing(Pairing {
                            ip: target.ip(),
                            name: name.clone(),
                            token,
                        });
                        self.pairing_error = None;
                        self.pin_input.clear();
                        // Keep the friendlier name now that we know it.
                        self.remember_target(target, &name);
                    }
                    Err(message) => self.pairing_error = Some(message),
                }
            }
        }

        ui.add_space(10.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("This receiver is not paired yet").strong());
            ui.label(
                egui::RichText::new(
                    "Read the six-digit PIN from the receiver's screen and type it here. \
                     You only do this once per machine.",
                )
                .color(egui::Color32::GRAY)
                .small(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_enabled(
                    !self.pairing_busy,
                    egui::TextEdit::singleline(&mut self.pin_input)
                        .hint_text("123 456")
                        .desired_width(110.0),
                );
                let pin = Pin::parse(&self.pin_input);
                if ui
                    .add_enabled(
                        pin.is_some() && !self.pairing_busy,
                        egui::Button::new("Pair"),
                    )
                    .clicked()
                {
                    if let Some(pin) = pin {
                        self.pairing_busy = true;
                        self.pairing_error = None;
                        // Off the GUI thread: it waits on the network.
                        let slot = Arc::clone(&self.pairing_result);
                        let ip = target.ip();
                        std::thread::spawn(move || {
                            let outcome = pair_with(ip, &pin);
                            if let Ok(mut slot) = slot.lock() {
                                *slot = Some(outcome);
                            }
                        });
                    }
                }
                if self.pairing_busy {
                    ui.spinner();
                }
            });
            if let Some(error) = &self.pairing_error {
                ui.colored_label(egui::Color32::from_rgb(235, 120, 100), error);
            }
        });
    }

    /// Receiver side: show the PIN that authorises a sender.
    fn pin_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("Pairing").strong());

            match self.pairing_host.display_pin() {
                Some(pin) => {
                    ui.label(
                        egui::RichText::new(pin)
                            .monospace()
                            .size(30.0)
                            .color(egui::Color32::from_rgb(120, 200, 255)),
                    );
                    let left = self
                        .pairing_host
                        .window_remaining()
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Type this on the sender. Open for {left}s."
                        ))
                        .color(egui::Color32::GRAY)
                        .small(),
                    );
                    if ui.small_button("Cancel").clicked() {
                        self.pairing_host.close_window();
                    }
                }
                None => {
                    if ui
                        .button("Pair a new device")
                        .on_hover_text(
                            "Shows a PIN for two minutes. Pairing is refused at every \
                             other moment, which is what keeps six digits safe.",
                        )
                        .clicked()
                    {
                        self.pairing_host.open_window();
                    }
                    ui.label(
                        egui::RichText::new(if self.streaming() {
                            "Machines already paired connect without a PIN."
                        } else {
                            "Press START first: a PIN is only answered while running."
                        })
                        .color(egui::Color32::GRAY)
                        .small(),
                    );
                }
            }

            if let Some(left) = self.pairing_host.locked_for() {
                ui.colored_label(
                    egui::Color32::from_rgb(230, 180, 60),
                    format!(
                        "Too many wrong attempts. Pairing is refused for {}s.",
                        left.as_secs()
                    ),
                );
            }

            let paired = self.settings.pairings.len();
            if paired > 0 {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{paired} paired machine{}",
                            if paired == 1 { "" } else { "s" }
                        ))
                        .color(egui::Color32::GRAY)
                        .small(),
                    );
                    if ui
                        .small_button("Revoke all")
                        .on_hover_text(
                            "Forgets every pairing on this machine. Each one must pair \
                             again before it can send audio or receive your microphone.",
                        )
                        .clicked()
                    {
                        self.settings.clear_pairings();
                    }
                });
            }
        });
    }

    /// Writes newly issued pairings to disk, so they survive a restart.
    fn drain_new_pairings(&mut self) {
        let fresh: Vec<_> = self
            .pairing_queue
            .lock()
            .map(|mut queue| queue.drain(..).collect())
            .unwrap_or_default();
        for pairing in fresh {
            self.settings.add_pairing(Pairing {
                ip: pairing.peer,
                name: pairing.peer.to_string(),
                token: pairing.token,
            });
        }
    }

    /// The return microphone stream.
    ///
    /// On the receiver this is "lend my microphone to the workstation"; on
    /// the sender it is "where do I play the microphone that arrives". They
    /// are two halves of one switch, so they share a section.
    fn mic_ui(&mut self, ui: &mut egui::Ui, is_sender: bool) {
        ui.add_space(12.0);
        let streaming = self.streaming();

        ui.add_enabled_ui(!streaming, |ui| {
            let label = if is_sender {
                "Use the receiver's microphone"
            } else {
                "Send this PC's microphone to the sender"
            };
            ui.checkbox(&mut self.mic_enabled, egui::RichText::new(label).strong());
        });

        if !self.mic_enabled {
            ui.label(
                egui::RichText::new(if is_sender {
                    "Off: nothing listens on the microphone port."
                } else {
                    "Off: this PC's microphone is never opened."
                })
                .color(egui::Color32::GRAY)
                .small(),
            );
            return;
        }

        // Device picker: an input endpoint on the receiver, an output
        // endpoint on the sender (because that is where the voice is played).
        let devices = if is_sender {
            &self.output_devices
        } else {
            &self.input_devices
        };
        let selected_id = if is_sender {
            &self.mic_output_device
        } else {
            &self.mic_input_device
        };
        let default_text = if is_sender {
            "Default output (follow Windows)"
        } else {
            "Default microphone (follow Windows)"
        };
        let selected_name = selected_id
            .as_ref()
            .and_then(|id| devices.iter().find(|d| &d.id == id).map(|d| d.name.clone()))
            .unwrap_or_else(|| default_text.to_string());

        let options: Vec<(String, String)> = devices
            .iter()
            .map(|d| (d.id.clone(), d.name.clone()))
            .collect();

        ui.add_enabled_ui(!streaming, |ui| {
            ui.horizontal(|ui| {
                ui.label(if is_sender { "Play it into" } else { "Microphone" });
                egui::ComboBox::from_id_salt(if is_sender { "mic-out" } else { "mic-in" })
                    .width(ui.available_width() - 8.0)
                    .selected_text(selected_name)
                    .show_ui(ui, |ui| {
                        let slot = if is_sender {
                            &mut self.mic_output_device
                        } else {
                            &mut self.mic_input_device
                        };
                        ui.selectable_value(slot, None, default_text);
                        for (id, name) in &options {
                            ui.selectable_value(slot, Some(id.clone()), name);
                        }
                    });
            });
        });

        if is_sender {
            ui.label(
                egui::RichText::new(
                    "Windows cannot expose a microphone from an ordinary app, so the voice \
                     is played into an output device. To use it as a microphone elsewhere, \
                     install VB-CABLE, play into \"CABLE Input\" here, and select \
                     \"CABLE Output\" as the microphone in OBS, Discord or your recorder.",
                )
                .color(egui::Color32::GRAY)
                .small(),
            );
        } else {
            ui.label(
                egui::RichText::new(
                    "The microphone opens only while streaming, and only with this box \
                     ticked. It is sent to whichever PC is streaming audio to this one.",
                )
                .color(egui::Color32::GRAY)
                .small(),
            );
        }

        if streaming {
            ui.add_space(4.0);
            if !is_sender {
                // Where the microphone is actually going. Latched for the
                // session on the first packet received, so it is worth
                // showing: a microphone whose destination is invisible is a
                // microphone you cannot trust.
                let destination = Shared::get_string(&self.mic_shared.peer)
                    .unwrap_or_else(|| "waiting for the sender...".to_string());
                ui.label(
                    egui::RichText::new(format!("Microphone going to {destination}"))
                        .color(egui::Color32::from_rgb(120, 170, 235))
                        .small(),
                );
            }
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Mic").small().color(egui::Color32::GRAY));
                let level = self.mic_shared.level().clamp(0.0, 1.0).powf(0.5);
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 10.0),
                    egui::Sense::hover(),
                );
                let radius = egui::CornerRadius::same(3);
                ui.painter()
                    .rect_filled(rect, radius, egui::Color32::from_gray(32));
                if level > 0.001 {
                    let mut filled = rect;
                    filled.set_width(rect.width() * level);
                    ui.painter().rect_filled(
                        filled,
                        radius,
                        egui::Color32::from_rgb(120, 170, 235),
                    );
                }
            });
            if let Some(error) = Shared::get_string(&self.mic_shared.audio_error) {
                ui.colored_label(egui::Color32::from_rgb(235, 120, 100), error);
            }
        }
    }

    fn latency_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Latency").strong());
        let streaming = self.streaming();
        ui.add_enabled_ui(!streaming, |ui| {
            ui.horizontal(|ui| {
                for preset in Latency::ALL {
                    ui.selectable_value(&mut self.preset, preset, preset.label());
                }
                ui.label(
                    egui::RichText::new(format!("~{:.0} ms", self.preset.estimate_ms()))
                        .color(egui::Color32::GRAY),
                );
            });
        });
        ui.label(
            egui::RichText::new(self.preset.hint())
                .color(egui::Color32::GRAY)
                .small(),
        );
        if streaming {
            ui.label(
                egui::RichText::new("Stop the stream to change the preset.")
                    .color(egui::Color32::DARK_GRAY)
                    .small(),
            );
        }
    }

    fn start_stop_ui(&mut self, ui: &mut egui::Ui, is_sender: bool) {
        ui.add_space(14.0);
        let streaming = self.streaming();
        let can_start = if is_sender {
            self.target
                .map(|t| self.settings.token_for(t.ip()).is_some())
                .unwrap_or(false)
        } else {
            true
        };

        let text = if streaming { "STOP" } else { "START" };
        let colour = if streaming {
            egui::Color32::from_rgb(170, 60, 55)
        } else {
            egui::Color32::from_rgb(40, 130, 90)
        };

        let button = egui::Button::new(egui::RichText::new(text).size(22.0).strong()).fill(colour);
        let response = ui.add_enabled_ui(streaming || can_start, |ui| {
            ui.add_sized([ui.available_width(), 56.0], button)
        });

        if response.inner.clicked() {
            if streaming {
                self.stop_session();
            } else if is_sender {
                if let (Some(target), Some(token)) =
                    (self.target, self.target.and_then(|t| self.settings.token_for(t.ip())))
                {
                    self.session = Some(Session::start_sender(
                        self.capture_device.clone(),
                        self.preset,
                        target,
                        token,
                        Arc::clone(&self.shared),
                        MicConfig {
                            enabled: self.mic_enabled,
                            device_id: self.mic_output_device.clone(),
                        },
                        Arc::clone(&self.mic_shared),
                    ));
                }
            } else {
                self.session = Some(Session::start_receiver(
                    self.playback_device.clone(),
                    self.preset,
                    AUDIO_PORT,
                    Arc::new(TokenStore::new(self.settings.all_tokens())),
                    Arc::clone(&self.pairing_host),
                    Arc::clone(&self.pairing_queue),
                    Arc::clone(&self.shared),
                    MicConfig {
                        enabled: self.mic_enabled,
                        device_id: self.mic_input_device.clone(),
                    },
                    Arc::clone(&self.mic_shared),
                ));
            }
        }

        if is_sender && !can_start {
            ui.label(
                egui::RichText::new(if self.target.is_some() {
                    "Pair with this receiver first."
                } else {
                    "Pick a receiver above first."
                })
                    .color(egui::Color32::GRAY)
                    .small(),
            );
        }
    }

    fn status_ui(&mut self, ui: &mut egui::Ui, is_sender: bool) {
        ui.add_space(12.0);
        ui.separator();

        let streaming = self.streaming();
        let live = self.shared.stream_live(STREAM_TIMEOUT_MS);
        let (dot, text) = if !streaming {
            (egui::Color32::DARK_GRAY, "Stopped".to_string())
        } else if live {
            let peer = Shared::get_string(&self.shared.peer).unwrap_or_default();
            let who = if is_sender {
                if self.target_label.is_empty() {
                    peer
                } else {
                    self.target_label.clone()
                }
            } else {
                peer
            };
            (
                egui::Color32::from_rgb(70, 200, 120),
                format!("Streaming — {who}"),
            )
        } else if is_sender {
            (
                egui::Color32::from_rgb(230, 180, 60),
                format!("Connecting to {}...", self.target_label),
            )
        } else {
            (
                egui::Color32::from_rgb(230, 180, 60),
                format!("Waiting for a sender on port {AUDIO_PORT}..."),
            )
        };

        ui.horizontal(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.5, dot);
            ui.label(egui::RichText::new(text).strong());
        });

        // Level meter: the quickest way to see whether audio is really moving.
        ui.add_space(4.0);
        self.level_meter(ui);

        if let Some(name) = Shared::get_string(&self.shared.device_name) {
            ui.label(
                egui::RichText::new(format!("Device: {name}"))
                    .color(egui::Color32::GRAY)
                    .small(),
            );
        }

        if streaming {
            let blocks = self.shared.blocks.load(Ordering::Relaxed);
            let lost = self.shared.lost.load(Ordering::Relaxed);
            let underruns = self.shared.underruns.load(Ordering::Relaxed);
            let late = self.shared.dropped_late.load(Ordering::Relaxed);
            let buffered = self.shared.buffer_blocks.load(Ordering::Relaxed);

            let mut stats = format!("{blocks} blocks");
            if !is_sender {
                let target = self.shared.target_blocks.load(Ordering::Relaxed);
                stats.push_str(&format!(
                    " · buffer {:.0}/{:.0} ms · {underruns} dropouts",
                    buffered as f32 * BLOCK_MS,
                    target as f32 * BLOCK_MS
                ));
            }
            if lost > 0 {
                stats.push_str(&format!(" · {lost} lost"));
            }
            if late > 0 {
                stats.push_str(&format!(" · {late} late"));
            }
            if self.shared.pro_audio.load(Ordering::Relaxed) {
                stats.push_str(" · Pro Audio");
            }
            ui.label(
                egui::RichText::new(stats)
                    .color(egui::Color32::GRAY)
                    .small(),
            );
        }

        for error in [
            Shared::get_string(&self.shared.audio_error),
            Shared::get_string(&self.shared.net_error),
        ]
        .into_iter()
        .flatten()
        {
            ui.colored_label(egui::Color32::from_rgb(235, 120, 100), error);
        }

        if is_sender && self.output_devices.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(235, 150, 60),
                "No output device found. Install VB-CABLE and set it as the default output \
                 so there is something to capture (see the README).",
            );
        }
    }

    /// A simple peak meter. Green up to -6 dBFS, amber above it.
    fn level_meter(&self, ui: &mut egui::Ui) {
        let height = 14.0;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        let radius = egui::CornerRadius::same(3);
        painter.rect_filled(rect, radius, egui::Color32::from_gray(32));

        let level = self.shared.level().clamp(0.0, 1.0);
        // A little curve so quiet material is still visible.
        let shown = level.powf(0.5);
        if shown > 0.001 {
            let mut filled = rect;
            filled.set_width(rect.width() * shown);
            let colour = if level > 0.5 {
                egui::Color32::from_rgb(230, 180, 60)
            } else {
                egui::Color32::from_rgb(70, 200, 120)
            };
            painter.rect_filled(filled, radius, colour);
        }
    }

    /// Restores or quits when the tray menu was used.
    fn handle_tray(&mut self, ctx: &egui::Context) {
        let Some(tray) = &self.tray else { return };
        if tray.take_show_request() {
            self.hidden = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if tray.take_quit_request() {
            self.stop_session();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Enumerates endpoints on a throwaway thread.
///
/// WASAPI needs COM, and the GUI thread belongs to winit; keeping the two
/// apart avoids apartment surprises and costs a couple of milliseconds.
fn list_devices(direction: Direction) -> Vec<DeviceInfo> {
    std::thread::spawn(move || {
        let _ = wasapi::initialize_mta().ok();
        devices::list(direction).unwrap_or_default()
    })
    .join()
    .unwrap_or_default()
}

/// Accepts "192.168.1.20" or "192.168.1.20:45778".
fn parse_target(text: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Type an address first.".into());
    }
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Ok(addr);
    }
    match text.parse::<IpAddr>() {
        Ok(ip) => Ok(SocketAddr::new(ip, AUDIO_PORT)),
        Err(_) => Err(format!(
            "\"{text}\" is not an IP address. Try something like 192.168.1.20"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_ip_and_ip_with_port() {
        assert_eq!(
            parse_target("192.168.1.20").unwrap(),
            SocketAddr::new("192.168.1.20".parse::<IpAddr>().unwrap(), AUDIO_PORT)
        );
        assert_eq!(
            parse_target(" 192.168.1.20:5000 ").unwrap().port(),
            5000
        );
        assert!(parse_target("laptop").is_err());
        assert!(parse_target("").is_err());
    }
}
