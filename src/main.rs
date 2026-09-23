mod export;
mod iperf;
mod models;
mod settings;

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use export::ExportContext;
use iperf::resolve_iperf3;
use iperf::runner::{IperfConfig, detect_iperf3, run_test};
use models::{TestEvent, TestSummary, ThroughputSample};
use settings::AppSettings;

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Primary accent: cyan. Used for the brand mark, primary actions, the live
/// badge, and the throughput line.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(34, 211, 238);
const ACCENT_DARK_TEXT: egui::Color32 = egui::Color32::from_rgb(8, 47, 73);
const BG: egui::Color32 = egui::Color32::from_rgb(13, 17, 23);
const CARD: egui::Color32 = egui::Color32::from_rgb(22, 27, 34);
const CARD_BORDER: egui::Color32 = egui::Color32::from_rgb(48, 54, 61);
const INPUT_BG: egui::Color32 = egui::Color32::from_rgb(13, 17, 23);
const TEXT: egui::Color32 = egui::Color32::from_rgb(230, 237, 243);
const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(139, 148, 158);
const DANGER: egui::Color32 = egui::Color32::from_rgb(248, 81, 73);
const SUCCESS: egui::Color32 = egui::Color32::from_rgb(63, 185, 80);

fn setup_theme(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = BG;
        style.visuals.extreme_bg_color = INPUT_BG;

        // Soft rounded corners on every interactive widget.
        for widget in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.corner_radius = egui::CornerRadius::same(8);
        }

        style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(33, 40, 48);
        style.visuals.selection.bg_fill = egui::Color32::from_rgb(8, 47, 73);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
    });
}

/// Rounded card frame used for every panel on screen.
fn card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::group(ui.style())
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0, CARD_BORDER))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::same(20))
}

fn section_title(text: &str) -> egui::RichText {
    egui::RichText::new(text).size(15.0).strong().color(TEXT)
}

fn field_label(text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .size(12.0)
        .strong()
        .color(TEXT_DIM)
}

struct NetworkSpeedApp {
    server: String,
    port: String,
    duration: String,

    iperf3_path: String,
    iperf3_version: Option<String>,
    iperf3_found: bool,
    /// The binary that will actually be executed. This is separate from
    /// `iperf3_path` (the user's text field) so a wrong path is reported
    /// as an error instead of being silently replaced by auto-detection.
    resolved_iperf3: Option<PathBuf>,
    /// Trimmed `iperf3_path` value the last detection ran against, so
    /// typing in the field re-runs detection exactly when the text changes.
    last_checked_iperf3: String,

    running: bool,

    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<TestEvent>>,

    samples: Vec<ThroughputSample>,
    summary: Option<TestSummary>,

    current_mbps: f64,
    status: String,
    error: Option<String>,
    export_message: Option<String>,

    test_started_at: Option<Instant>,
    elapsed_seconds: f64,
}

impl Default for NetworkSpeedApp {
    fn default() -> Self {
        let stored = AppSettings::load();

        let mut app = Self {
            server: "127.0.0.1".to_string(),
            port: "5201".to_string(),
            duration: "10".to_string(),

            iperf3_path: stored.iperf3_path.unwrap_or_default(),
            iperf3_version: None,
            iperf3_found: false,
            resolved_iperf3: None,
            last_checked_iperf3: String::new(),

            running: false,

            cancel: None,
            receiver: None,

            samples: Vec::new(),
            summary: None,

            current_mbps: 0.0,
            status: "Ready".to_string(),
            error: None,
            export_message: None,

            test_started_at: None,
            elapsed_seconds: 0.0,
        };

        // Startup detection: custom path -> bundled copy -> PATH.
        app.refresh_iperf3();

        app
    }
}

impl NetworkSpeedApp {
    fn format_speed(mbps: f64) -> String {
        if mbps >= 1000.0 {
            format!("{:.2} Gbps", mbps / 1000.0)
        } else {
            format!("{:.2} Mbps", mbps)
        }
    }

    fn format_bytes(bytes: u64) -> String {
        const KB: f64 = 1024.0;
        const MB: f64 = 1024.0 * 1024.0;
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;

        let value = bytes as f64;

        if value >= GB {
            format!("{:.2} GB", value / GB)
        } else if value >= MB {
            format!("{:.2} MB", value / MB)
        } else if value >= KB {
            format!("{:.2} KB", value / KB)
        } else {
            format!("{} bytes", bytes)
        }
    }

    fn reset_results(&mut self) {
        self.samples.clear();
        self.summary = None;
        self.current_mbps = 0.0;

        self.error = None;
        self.export_message = None;
        self.elapsed_seconds = 0.0;
        self.test_started_at = None;
    }

    /// Drop a stale error once the user edits the configuration, so a
    /// previous failure (e.g. a bad port) does not linger after the input
    /// that caused it has been fixed.
    fn clear_error_state(&mut self) {
        self.error = None;
        if !self.running && self.status == "Error" {
            self.status = "Ready".to_string();
        }
    }

    /// Re-resolve the iperf3 executable and update the detection state.
    ///
    /// A path typed into the text field is honored as-is: if it does not
    /// point at a file, detection reports "not found" and the field is
    /// left untouched so the user sees the error. Auto-detection
    /// (bundled copy -> PATH) only runs when the field is empty.
    fn refresh_iperf3(&mut self) {
        let trimmed = self.iperf3_path.trim().to_string();
        self.last_checked_iperf3 = trimmed.clone();

        if !trimmed.is_empty() {
            let path = PathBuf::from(&trimmed);

            if path.is_file() {
                match detect_iperf3(&trimmed) {
                    Ok(version) => {
                        self.resolved_iperf3 = Some(path);
                        self.iperf3_version = Some(version);
                        self.iperf3_found = true;
                    }
                    Err(_) => {
                        self.resolved_iperf3 = None;
                        self.iperf3_version = None;
                        self.iperf3_found = false;
                    }
                }
            } else {
                // Explicit but invalid: surface an error, never fall back
                // to the auto-detected binary behind the user's back.
                self.resolved_iperf3 = None;
                self.iperf3_version = None;
                self.iperf3_found = false;
            }

            return;
        }

        match resolve_iperf3(None) {
            Some(path) => match detect_iperf3(&path.to_string_lossy()) {
                Ok(version) => {
                    self.resolved_iperf3 = Some(path);
                    self.iperf3_version = Some(version);
                    self.iperf3_found = true;
                }
                Err(_) => {
                    self.resolved_iperf3 = None;
                    self.iperf3_version = None;
                    self.iperf3_found = false;
                }
            },
            None => {
                self.resolved_iperf3 = None;
                self.iperf3_version = None;
                self.iperf3_found = false;
            }
        }
    }

    fn save_iperf3_setting(&self) {
        let trimmed = self.iperf3_path.trim();

        AppSettings {
            iperf3_path: if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            },
        }
        .save();
    }

    /// Pure field checks, shared by the inline form hints and `validate()`
    /// so the messages stay identical in both places.
    fn server_error(&self) -> Option<String> {
        if self.server.trim().is_empty() {
            Some("Server is required.".to_string())
        } else {
            None
        }
    }

    fn port_error(&self) -> Option<String> {
        match self.port.trim().parse::<u16>() {
            Ok(port) if port > 0 => None,
            _ => Some(format!(
                "\"{}\" is not a valid port. Enter a number from 1 to 65535.",
                self.port.trim()
            )),
        }
    }

    fn duration_error(&self) -> Option<String> {
        match self.duration.trim().parse::<u32>() {
            Ok(duration) if duration > 0 => None,
            _ => Some(format!(
                "\"{}\" is not a valid duration. Enter a whole number of seconds greater than 0.",
                self.duration.trim()
            )),
        }
    }

    fn iperf3_error(&self) -> Option<String> {
        if self.iperf3_found {
            return None;
        }

        let custom = self.iperf3_path.trim();

        if custom.is_empty() {
            Some(
                "iperf3 was not found. Install iperf3 or choose its location with Browse."
                    .to_string(),
            )
        } else {
            Some(format!(
                "iperf3 was not found at \"{custom}\". Fix the path or clear the field to auto-detect."
            ))
        }
    }

    fn validate(&mut self) -> Option<(u16, u32)> {
        self.refresh_iperf3();

        if let Some(error) = self
            .server_error()
            .or_else(|| self.port_error())
            .or_else(|| self.duration_error())
            .or_else(|| self.iperf3_error())
        {
            self.error = Some(error);
            return None;
        }

        // The checks above already parsed these successfully.
        let port = self.port.trim().parse::<u16>().ok()?;
        let duration = self.duration.trim().parse::<u32>().ok()?;

        Some((port, duration))
    }

    fn start_test(&mut self) {
        let Some((port, duration)) = self.validate() else {
            return;
        };

        let (sender, receiver) = mpsc::channel();

        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);

        // `validate()` guarantees detection succeeded, so this is always
        // `Some` here; fall back to the raw field only defensively.
        let executable = self
            .resolved_iperf3
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| self.iperf3_path.trim().to_string());

        let config = IperfConfig {
            executable,
            server: self.server.trim().to_string(),
            port,
            duration_seconds: duration,
        };

        self.reset_results();

        // Remember the working iperf3 location for the next launch.
        self.save_iperf3_setting();

        thread::spawn(move || {
            run_test(config, sender, worker_cancel);
        });

        self.receiver = Some(receiver);
        self.cancel = Some(cancel);
        self.status = "Starting...".to_string();
        self.running = true;
        self.test_started_at = Some(Instant::now());
    }

    fn cancel_test(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
            self.status = "Cancelling...".to_string();
        }
    }

    fn process_events(&mut self) {
        let events: Vec<TestEvent> = match &self.receiver {
            Some(receiver) => receiver.try_iter().collect(),
            None => return,
        };

        for event in events {
            match event {
                TestEvent::Started => {
                    self.status = "Running".to_string();
                }

                TestEvent::Throughput(sample) => {
                    self.current_mbps = sample.bits_per_second / 1_000_000.0;
                    self.samples.push(sample);
                }

                TestEvent::Finished(summary) => {
                    self.running = false;
                    self.status = "Finished".to_string();
                    self.summary = Some(summary);
                    self.cancel = None;
                }

                TestEvent::Error(error) => {
                    self.running = false;
                    self.status = "Error".to_string();
                    self.error = Some(error);
                    self.cancel = None;
                }

                TestEvent::Cancelled => {
                    self.running = false;
                    self.status = "Cancelled".to_string();
                    self.cancel = None;
                }
            }
        }
    }

    fn update_elapsed(&mut self) {
        if self.running
            && let Some(started) = self.test_started_at
        {
            self.elapsed_seconds = started.elapsed().as_secs_f64();
        }
    }

    fn export_results(&mut self, format: &str) {
        let (extension, filter_name) = match format {
            "csv" => ("csv", "CSV"),
            "json" => ("json", "JSON"),
            _ => ("txt", "Text"),
        };

        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("voyis-speedtest.{}", extension))
            .add_filter(filter_name, &[extension])
            .save_file()
        else {
            return;
        };

        // Clone the small summary so the export borrow doesn't conflict
        // with updating the status message below.
        let summary = self.summary.clone().unwrap_or_default();
        let contents = {
            let ctx = ExportContext {
                server: &self.server,
                port: self.port.trim().parse().unwrap_or(5201),
                duration_seconds: self.duration.trim().parse().unwrap_or(0),
                samples: &self.samples,
                summary: &summary,
            };
            match format {
                "csv" => Ok(export::to_csv(&ctx)),
                "json" => export::to_json(&ctx),
                _ => Ok(export::to_txt(&ctx)),
            }
        };

        self.export_message = match contents {
            Ok(contents) => match export::write_file(&path, &contents) {
                Ok(()) => Some(format!("Saved to {}", path.display())),
                Err(error) => Some(format!("Export failed: {error}")),
            },
            Err(error) => Some(format!("Export failed: {error}")),
        };
    }

    fn show_header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("VOYIS")
                    .strong()
                    .size(22.0)
                    .color(ACCENT),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Network Speed Test")
                    .size(17.0)
                    .color(TEXT),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.show_status_pill(ui);
            });
        });
    }

    /// Rounded status badge with a colored dot: Ready / Running / Finished /
    /// Cancelled / Cancelling... / Error.
    fn show_status_pill(&self, ui: &mut egui::Ui) {
        let (dot, bg) = match self.status.as_str() {
            "Running" => (ACCENT, egui::Color32::from_rgb(8, 47, 73)),
            "Finished" => (SUCCESS, egui::Color32::from_rgb(12, 45, 28)),
            "Error" => (DANGER, egui::Color32::from_rgb(52, 18, 16)),
            "Cancelled" | "Cancelling..." => (
                egui::Color32::from_rgb(210, 153, 34),
                egui::Color32::from_rgb(52, 40, 10),
            ),
            _ => (TEXT_DIM, egui::Color32::from_rgb(33, 38, 45)),
        };

        egui::Frame::group(ui.style())
            .fill(bg)
            .corner_radius(egui::CornerRadius::same(16))
            .inner_margin(egui::Margin::symmetric(14, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(egui::RichText::new("●").size(10.0).color(dot));
                    ui.label(
                        egui::RichText::new(&self.status)
                            .size(13.0)
                            .strong()
                            .color(TEXT),
                    );
                });
            });
    }

    fn show_configuration(&mut self, ui: &mut egui::Ui) {
        // Re-run detection when the path text changed since the last check,
        // so typing a wrong path immediately surfaces an error instead of
        // silently falling back to the auto-detected binary.
        if self.iperf3_path.trim() != self.last_checked_iperf3 {
            self.refresh_iperf3();
        }

        ui.label(section_title("Test Configuration"));
        ui.add_space(12.0);

        // If the user edits any field after an error, the old error is
        // stale: clear it (and the Error pill) so fixed inputs read as
        // Ready again.
        let mut inputs_changed = false;

        ui.add_enabled_ui(!self.running, |ui| {
            ui.label(field_label("SERVER"));
            if ui
                .add(egui::TextEdit::singleline(&mut self.server).desired_width(f32::INFINITY))
                .changed()
            {
                inputs_changed = true;
            }
            if let Some(hint) = self.server_error() {
                ui.label(egui::RichText::new(hint).small().color(DANGER));
            }
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(field_label("PORT"));
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.port).desired_width(110.0))
                        .changed()
                    {
                        inputs_changed = true;
                    }
                    if let Some(hint) = self.port_error() {
                        ui.label(egui::RichText::new(hint).small().color(DANGER));
                    }
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(field_label("DURATION"));
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::TextEdit::singleline(&mut self.duration).desired_width(80.0))
                            .changed()
                        {
                            inputs_changed = true;
                        }
                        ui.label(egui::RichText::new("seconds").color(TEXT_DIM));
                    });
                    if let Some(hint) = self.duration_error() {
                        ui.label(egui::RichText::new(hint).small().color(DANGER));
                    }
                });
            });
            ui.add_space(10.0);

            ui.label(field_label("IPERF3 EXECUTABLE"));
            ui.horizontal(|ui| {
                let available = ui.available_width() - 92.0;
                if ui
                    .add(egui::TextEdit::singleline(&mut self.iperf3_path).desired_width(available))
                    .changed()
                {
                    inputs_changed = true;
                }
                if ui.button("Browse...").clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_file()
                {
                    self.iperf3_path = path.to_string_lossy().to_string();
                    self.refresh_iperf3();
                    self.save_iperf3_setting();
                    self.clear_error_state();
                }
            });
        });

        if inputs_changed {
            self.clear_error_state();
        }

        ui.add_space(10.0);

        let custom_path = self.iperf3_path.trim().to_string();

        if self.iperf3_found {
            let mut status = self
                .iperf3_version
                .clone()
                .unwrap_or_else(|| "iperf3 detected".to_string());

            // When auto-detecting (field empty), show which binary will run.
            if custom_path.is_empty()
                && let Some(resolved) = &self.resolved_iperf3
            {
                status.push_str(&format!("  •  {}", resolved.to_string_lossy()));
            }

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("●").size(9.0).color(SUCCESS));
                ui.label(egui::RichText::new(status).small().color(TEXT_DIM));
            });
        } else if !custom_path.is_empty() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("●").size(9.0).color(DANGER));
                ui.label(
                    egui::RichText::new(format!(
                        "iperf3 not found at \"{custom_path}\". Fix the path or clear the field to auto-detect."
                    ))
                    .small()
                    .color(DANGER),
                );
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("●").size(9.0).color(DANGER));
                ui.label(
                    egui::RichText::new(
                        "iperf3 not detected — install it or pick its location with Browse.",
                    )
                    .small()
                    .color(DANGER),
                );
            });
        }

        ui.add_space(16.0);

        if !self.running {
            ui.add_enabled_ui(self.iperf3_found, |ui| {
                let button = egui::Button::new(
                    egui::RichText::new("▶   Start Test")
                        .size(15.0)
                        .strong()
                        .color(ACCENT_DARK_TEXT),
                )
                .fill(ACCENT)
                .corner_radius(egui::CornerRadius::same(10));
                if ui.add_sized([ui.available_width(), 46.0], button).clicked() {
                    self.start_test();
                }
            });
            if !self.iperf3_found {
                let hint = self
                    .iperf3_error()
                    .unwrap_or_else(|| "Start is disabled until iperf3 is found.".to_string());
                ui.label(egui::RichText::new(hint).small().color(DANGER));
            }
        } else {
            let button = egui::Button::new(
                egui::RichText::new("■   Cancel")
                    .size(15.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            )
            .fill(DANGER)
            .corner_radius(egui::CornerRadius::same(10));
            if ui.add_sized([ui.available_width(), 46.0], button).clicked() {
                self.cancel_test();
            }
        }

        // Surface the latest error right under the action button so it is
        // always visible without scrolling. This covers both validation
        // failures (e.g. a bad port) and runtime failures (e.g. connection
        // refused because nothing listens on the chosen port).
        if let Some(error) = self.error.clone() {
            ui.add_space(10.0);

            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(52, 18, 16))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(128, 32, 28)))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(14))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(error).color(egui::Color32::from_rgb(255, 180, 170)),
                    );
                });
        }
    }

    /// Split "161.00 Gbps" into ("161.00", "Gbps") for the hero readout.
    fn split_speed(mbps: f64) -> (String, String) {
        if mbps >= 1000.0 {
            (format!("{:.2}", mbps / 1000.0), "Gbps".to_string())
        } else {
            (format!("{:.2}", mbps), "Mbps".to_string())
        }
    }

    fn show_current_speed(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(section_title("Current Speed"));
            if self.running {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("● LIVE")
                            .size(12.0)
                            .strong()
                            .color(ACCENT),
                    );
                });
            }
        });

        ui.add_space(14.0);

        ui.vertical_centered(|ui| {
            let (value, unit) = Self::split_speed(self.current_mbps);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(value).size(52.0).strong().color(
                    if self.samples.is_empty() {
                        TEXT_DIM
                    } else {
                        TEXT
                    },
                ));
                ui.label(egui::RichText::new(unit).size(20.0).color(TEXT_DIM));
            });

            ui.add_space(6.0);

            if self.samples.is_empty() {
                ui.label(egui::RichText::new("Waiting for throughput data...").color(TEXT_DIM));
            } else {
                let live_bytes: u64 = self
                    .samples
                    .iter()
                    .map(|sample| sample.transfer_bytes)
                    .sum();

                let live_retransmits: u64 = self
                    .samples
                    .iter()
                    .filter_map(|sample| sample.retransmits)
                    .sum();

                ui.label(
                    egui::RichText::new(format!(
                        "{} transferred  •  {} retransmits",
                        Self::format_bytes(live_bytes),
                        live_retransmits
                    ))
                    .small()
                    .color(TEXT_DIM),
                );
            }
        });

        if self.running {
            ui.add_space(18.0);

            let duration = self.duration.parse::<f32>().unwrap_or(1.0);
            // Never display more elapsed time than the requested duration,
            // so a run that overruns (e.g. while a hung iperf3 is being
            // timed out) doesn't read as "21.8s / 10s".
            let elapsed = (self.elapsed_seconds as f32).min(duration.max(0.001));
            let progress = (elapsed / duration).clamp(0.0, 1.0);

            let bar = egui::ProgressBar::new(progress)
                .desired_width(ui.available_width())
                .desired_height(22.0)
                .text(format!("{:.1}s / {}s", elapsed, self.duration))
                .fill(ACCENT);

            ui.add(bar);
        }
    }

    fn show_graph(&self, ui: &mut egui::Ui) {
        ui.label(section_title("Live Throughput"));

        ui.add_space(8.0);

        if self.samples.is_empty() {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 260.0),
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    ui.label(
                        egui::RichText::new(
                            "No throughput samples yet.\n\nStart a test to see live network performance.",
                        )
                        .size(15.0)
                        .color(TEXT_DIM),
                    );
                },
            );

            return;
        }

        let points: PlotPoints = self
            .samples
            .iter()
            .map(|sample| {
                [
                    (sample.start_seconds + sample.end_seconds) / 2.0,
                    sample.bits_per_second / 1_000_000.0,
                ]
            })
            .collect();

        let line = Line::new("Throughput", points).color(ACCENT).width(2.5);

        Plot::new("throughput_plot")
            .height(260.0)
            .x_axis_label("Time (seconds)")
            .y_axis_label("Mbps")
            .allow_zoom(false)
            .allow_drag(false)
            .show(ui, |plot_ui| {
                plot_ui.line(line);
            });
    }

    fn show_results(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(section_title("Results"));

            if self.status == "Finished" {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(message) = self.export_message.clone() {
                        ui.label(egui::RichText::new(message).small().color(TEXT_DIM));
                    }
                    for (label, format) in [("Report", "txt"), ("JSON", "json"), ("CSV", "csv")] {
                        if ui.button(label).clicked() {
                            self.export_results(format);
                        }
                    }
                    ui.label(egui::RichText::new("Export").small().color(TEXT_DIM));
                });
            }
        });

        ui.add_space(10.0);

        // Fall back to values calculated from live samples.
        let live_bytes: u64 = self
            .samples
            .iter()
            .map(|sample| sample.transfer_bytes)
            .sum();

        let live_retransmits: u64 = self
            .samples
            .iter()
            .filter_map(|sample| sample.retransmits)
            .sum();

        let mbps = |bps: Option<f64>| {
            bps.map(|value| Self::format_speed(value / 1_000_000.0))
                .unwrap_or_else(|| "—".to_string())
        };

        let sender = mbps(
            self.summary
                .as_ref()
                .and_then(|summary| summary.sender_bits_per_second),
        );
        let receiver = mbps(
            self.summary
                .as_ref()
                .and_then(|summary| summary.receiver_bits_per_second),
        );

        let sent = self
            .summary
            .as_ref()
            .and_then(|summary| summary.sent_bytes)
            .map(Self::format_bytes)
            .unwrap_or_else(|| {
                if self.samples.is_empty() {
                    "—".to_string()
                } else {
                    Self::format_bytes(live_bytes)
                }
            });

        let received = self
            .summary
            .as_ref()
            .and_then(|summary| summary.received_bytes)
            .map(Self::format_bytes)
            .unwrap_or_else(|| "—".to_string());

        let retransmits = self
            .summary
            .as_ref()
            .and_then(|summary| summary.retransmits)
            .map(|value| value.to_string())
            .unwrap_or_else(|| {
                if self.samples.is_empty() {
                    "—".to_string()
                } else {
                    live_retransmits.to_string()
                }
            });

        ui.columns(5, |columns| {
            self.result_card(&mut columns[0], "SENDER", sender);
            self.result_card(&mut columns[1], "RECEIVER", receiver);
            self.result_card(&mut columns[2], "SENT", sent);
            self.result_card(&mut columns[3], "RECEIVED", received);
            self.result_card(&mut columns[4], "RETRANSMITS", retransmits);
        });
    }

    fn result_card(&self, ui: &mut egui::Ui, title: &str, value: String) {
        egui::Frame::group(ui.style())
            .fill(CARD)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(title)
                            .size(11.0)
                            .strong()
                            .color(TEXT_DIM),
                    );

                    ui.add_space(10.0);

                    ui.label(egui::RichText::new(value).size(21.0).strong().color(TEXT));
                });
            });
    }
}

impl eframe::App for NetworkSpeedApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.process_events();
        self.update_elapsed();

        if self.running {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        ui.add_space(16.0);

        self.show_header(ui);

        ui.add_space(16.0);

        ui.columns(2, |columns| {
            card(&columns[0]).show(&mut columns[0], |ui| {
                self.show_configuration(ui);
            });

            card(&columns[1]).show(&mut columns[1], |ui| {
                self.show_current_speed(ui);
            });
        });

        ui.add_space(16.0);

        card(ui).show(ui, |ui| {
            self.show_graph(ui);
        });

        ui.add_space(16.0);

        ui.add_space(10.0);

        self.show_results(ui);

        ui.add_space(12.0);
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 750.0])
            .with_min_inner_size([900.0, 650.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Voyis Network Speed Test",
        options,
        Box::new(|cc| {
            setup_theme(&cc.egui_ctx);
            Ok(Box::new(NetworkSpeedApp::default()))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> NetworkSpeedApp {
        NetworkSpeedApp::default()
    }

    #[test]
    fn wrong_iperf3_path_is_reported_not_silently_replaced() {
        let mut app = test_app();
        app.iperf3_path = "/definitely/not/here/iperf3".to_string();
        app.refresh_iperf3();

        assert!(!app.iperf3_found);
        assert!(app.resolved_iperf3.is_none());

        // The user's text must be preserved, not overwritten with the
        // auto-detected binary.
        assert_eq!(app.iperf3_path, "/definitely/not/here/iperf3");

        let error = app.iperf3_error().expect("expected an iperf3 error");
        assert!(
            error.contains("/definitely/not/here/iperf3"),
            "error should name the bad path, got: {error}"
        );
    }

    #[test]
    fn validate_rejects_bad_port_with_helpful_message() {
        let mut app = test_app();
        app.port = "abc".to_string();

        assert!(app.validate().is_none());

        let error = app.error.clone().expect("expected a validation error");
        assert!(
            error.contains("abc"),
            "error should echo the input: {error}"
        );
        assert!(
            error.contains("1 to 65535"),
            "error should give the valid range: {error}"
        );
    }

    #[test]
    fn validate_rejects_out_of_range_and_zero_ports() {
        for bad in ["0", "70000", "-1", ""] {
            let mut app = test_app();
            app.port = bad.to_string();

            assert!(app.validate().is_none(), "port {bad:?} should be rejected");

            let error = app.error.clone().expect("expected a validation error");
            assert!(
                error.contains("1 to 65535"),
                "port {bad:?}: expected range hint, got: {error}"
            );
        }
    }

    #[test]
    fn validate_rejects_bad_duration_with_helpful_message() {
        for bad in ["0", "-5", "ten", ""] {
            let mut app = test_app();
            app.duration = bad.to_string();

            assert!(
                app.validate().is_none(),
                "duration {bad:?} should be rejected"
            );

            let error = app.error.clone().expect("expected a validation error");
            assert!(
                error.contains("duration"),
                "duration {bad:?}: expected a duration hint, got: {error}"
            );
        }
    }

    #[test]
    fn validate_rejects_empty_server() {
        let mut app = test_app();
        app.server = "   ".to_string();

        assert!(app.validate().is_none());

        let error = app.error.clone().expect("expected a validation error");
        assert!(error.contains("Server is required"), "got: {error}");
    }

    #[test]
    fn clearing_error_state_resets_stale_error() {
        let mut app = test_app();
        app.error = Some("connection refused".to_string());
        app.status = "Error".to_string();

        app.clear_error_state();

        assert!(app.error.is_none());
        assert_eq!(app.status, "Ready");
    }

    #[test]
    fn inline_hints_agree_with_validate() {
        let app = test_app();

        // With default valid values there are no hints.
        assert!(app.server_error().is_none());
        assert!(app.port_error().is_none());
        assert!(app.duration_error().is_none());
    }

    /// Builds a tiny fake executable (a shell script answering
    /// `--version`) so detection succeeds without a real iperf3 binary.
    /// A script is used instead of a system binary like `/bin/true`
    /// because those are not guaranteed to exist on every Unix
    /// (macOS ships no `/bin/true`).
    #[cfg(unix)]
    #[test]
    fn validate_accepts_good_input() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("voyis-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let fake = dir.join("fake-iperf3");
        let mut script = std::fs::File::create(&fake).expect("create fake iperf3");
        writeln!(script, "#!/bin/sh").unwrap();
        writeln!(script, "echo 'iperf 3.21 (fake)'").unwrap();
        drop(script);
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake iperf3");

        let mut app = test_app();
        app.iperf3_path = fake.to_string_lossy().to_string();

        let result = app.validate();

        assert_eq!(result, Some((5201, 10)));
        assert!(app.error.is_none());
        assert!(app.iperf3_found);
        assert_eq!(
            app.resolved_iperf3,
            Some(fake.clone()),
            "the explicit path must be the resolved binary, not a fallback"
        );
        assert_eq!(
            app.iperf3_version.as_deref(),
            Some("iperf 3.21 (fake)"),
            "the version line should come from the fake binary"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
