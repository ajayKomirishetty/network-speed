mod export;
mod iperf;
mod models;
mod settings;

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

    /// Re-resolve the iperf3 executable and update the detection state.
    /// Priority: the path in the text field -> bundled copy -> PATH.
    fn refresh_iperf3(&mut self) {
        let trimmed = self.iperf3_path.trim();

        if !trimmed.is_empty() {
            let path = std::path::PathBuf::from(trimmed);
            if path.is_file() {
                match detect_iperf3(trimmed) {
                    Ok(version) => {
                        self.iperf3_version = Some(version);
                        self.iperf3_found = true;
                        return;
                    }
                    Err(_) => {
                        self.iperf3_version = None;
                        self.iperf3_found = false;
                        return;
                    }
                }
            }
        }

        match resolve_iperf3(None) {
            Some(path) => {
                let resolved = path.to_string_lossy().to_string();
                match detect_iperf3(&resolved) {
                    Ok(version) => {
                        // Show the effective binary so the user sees what runs.
                        // This fallback discovery is not persisted as a custom
                        // path; it is re-resolved on every startup.
                        self.iperf3_path = resolved;
                        self.iperf3_version = Some(version);
                        self.iperf3_found = true;
                    }
                    Err(_) => {
                        self.iperf3_version = None;
                        self.iperf3_found = false;
                    }
                }
            }
            None => {
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

    fn validate(&mut self) -> Option<(u16, u32)> {
        self.refresh_iperf3();

        let server = self.server.trim();

        if server.is_empty() {
            self.error = Some("Server is required.".to_string());
            return None;
        }

        let port = match self.port.trim().parse::<u16>() {
            Ok(port) if port > 0 => port,
            _ => {
                self.error = Some("Port must be between 1 and 65535.".to_string());
                return None;
            }
        };

        let duration = match self.duration.trim().parse::<u32>() {
            Ok(duration) if duration > 0 => duration,
            _ => {
                self.error = Some("Duration must be greater than 0.".to_string());
                return None;
            }
        };

        if !self.iperf3_found {
            self.error = Some(
                "iperf3 was not found. Install iperf3 or choose its location with Browse."
                    .to_string(),
            );
            return None;
        }

        Some((port, duration))
    }

    fn start_test(&mut self) {
        let Some((port, duration)) = self.validate() else {
            return;
        };

        let (sender, receiver) = mpsc::channel();

        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);

        let config = IperfConfig {
            executable: self.iperf3_path.trim().to_string(),
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
        ui.label(section_title("Test Configuration"));
        ui.add_space(12.0);

        ui.add_enabled_ui(!self.running, |ui| {
            ui.label(field_label("SERVER"));
            ui.add(egui::TextEdit::singleline(&mut self.server).desired_width(f32::INFINITY));
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(field_label("PORT"));
                    ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(110.0));
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(field_label("DURATION"));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.duration).desired_width(80.0));
                        ui.label(egui::RichText::new("seconds").color(TEXT_DIM));
                    });
                });
            });
            ui.add_space(10.0);

            ui.label(field_label("IPERF3 EXECUTABLE"));
            ui.horizontal(|ui| {
                let available = ui.available_width() - 92.0;
                ui.add(egui::TextEdit::singleline(&mut self.iperf3_path).desired_width(available));
                if ui.button("Browse...").clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_file()
                {
                    self.iperf3_path = path.to_string_lossy().to_string();
                    self.refresh_iperf3();
                    self.save_iperf3_setting();
                }
            });
        });

        ui.add_space(10.0);

        if let Some(version) = &self.iperf3_version {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("●").size(9.0).color(SUCCESS));
                ui.label(egui::RichText::new(version).small().color(TEXT_DIM));
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("●").size(9.0).color(DANGER));
                ui.label(
                    egui::RichText::new("iperf3 not detected — pick its location with Browse.")
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
                ui.label(
                    egui::RichText::new("Start is disabled until iperf3 is found.")
                        .small()
                        .color(TEXT_DIM),
                );
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
            let progress = (self.elapsed_seconds as f32 / duration).clamp(0.0, 1.0);

            let bar = egui::ProgressBar::new(progress)
                .desired_width(ui.available_width())
                .desired_height(22.0)
                .text(format!("{:.1}s / {}s", self.elapsed_seconds, self.duration))
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
