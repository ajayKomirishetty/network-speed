mod iperf;
mod models;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use iperf::runner::{IperfConfig, detect_iperf3, run_test};
use models::{TestEvent, ThroughputSample};

struct NetworkSpeedApp {
    server: String,
    port: String,
    duration: String,

    iperf3_path: String,
    iperf3_version: Option<String>,

    running: bool,

    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<TestEvent>>,

    samples: Vec<ThroughputSample>,

    current_mbps: f64,
    status: String,
    error: Option<String>,

    sender_mbps: Option<f64>,
    receiver_mbps: Option<f64>,
    total_bytes: Option<u64>,
    retransmits: Option<u64>,

    test_started_at: Option<Instant>,
    elapsed_seconds: f64,
}

impl Default for NetworkSpeedApp {
    fn default() -> Self {
        Self {
            server: "127.0.0.1".to_string(),
            port: "5201".to_string(),
            duration: "10".to_string(),

            iperf3_path: "iperf3".to_string(),
            iperf3_version: detect_iperf3("iperf3").ok(),

            running: false,

            cancel: None,
            receiver: None,

            samples: Vec::new(),

            current_mbps: 0.0,
            status: "Ready".to_string(),
            error: None,

            sender_mbps: None,
            receiver_mbps: None,
            total_bytes: None,
            retransmits: None,

            test_started_at: None,
            elapsed_seconds: 0.0,
        }
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
        self.current_mbps = 0.0;

        self.sender_mbps = None;
        self.receiver_mbps = None;
        self.total_bytes = None;
        self.retransmits = None;

        self.error = None;
        self.elapsed_seconds = 0.0;
        self.test_started_at = None;
    }

    fn validate(&mut self) -> Option<(u16, u32)> {
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

        if self.iperf3_path.trim().is_empty() {
            self.error = Some("iperf3 path is required.".to_string());
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

                    self.sender_mbps = summary
                        .sender_bits_per_second
                        .map(|value| value / 1_000_000.0);

                    self.receiver_mbps = summary
                        .receiver_bits_per_second
                        .map(|value| value / 1_000_000.0);

                    self.total_bytes = summary.total_bytes;
                    self.retransmits = summary.retransmits;

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
        if self.running {
            if let Some(started) = self.test_started_at {
                self.elapsed_seconds = started.elapsed().as_secs_f64();
            }
        }
    }

    fn status_color(&self) -> egui::Color32 {
        match self.status.as_str() {
            "Running" => egui::Color32::from_rgb(46, 204, 113),
            "Finished" => egui::Color32::from_rgb(52, 152, 219),
            "Error" => egui::Color32::from_rgb(231, 76, 60),
            "Cancelled" | "Cancelling..." => egui::Color32::from_rgb(241, 196, 15),
            _ => egui::Color32::GRAY,
        }
    }

    fn card_frame() -> egui::Frame {
        egui::Frame::group(&egui::Style::default()).inner_margin(egui::Margin::same(12))
    }

    fn show_header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new("VOYIS").strong().size(24.0));

            ui.separator();

            ui.label(
                egui::RichText::new("Network Speed Test")
                    .size(20.0)
                    .strong(),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(self.status_color(), egui::RichText::new("●").size(14.0));

                    ui.label(egui::RichText::new(&self.status).strong());
                });
            });
        });
    }

    fn show_configuration(&mut self, ui: &mut egui::Ui) {
        ui.heading(
            egui::RichText::new("Test Configuration")
                .size(17.0)
                .strong(),
        );

        ui.add_space(8.0);

        ui.add_enabled_ui(!self.running, |ui| {
            egui::Grid::new("configuration_grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label("Server");
                    ui.add(egui::TextEdit::singleline(&mut self.server).desired_width(210.0));
                    ui.end_row();

                    ui.label("Port");
                    ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(210.0));
                    ui.end_row();

                    ui.label("Duration");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.duration).desired_width(80.0));
                        ui.label("seconds");
                    });
                    ui.end_row();

                    ui.label("iperf3");
                    ui.add(egui::TextEdit::singleline(&mut self.iperf3_path).desired_width(210.0));
                    ui.end_row();
                });
        });

        ui.add_space(8.0);

        if let Some(version) = &self.iperf3_version {
            ui.label(egui::RichText::new(version).small().weak());
        } else {
            ui.colored_label(egui::Color32::from_rgb(231, 76, 60), "iperf3 not detected");
        }

        ui.add_space(10.0);

        if !self.running {
            if ui
                .add_sized(
                    [140.0, 34.0],
                    egui::Button::new(egui::RichText::new("▶  Start Test").strong()),
                )
                .clicked()
            {
                self.start_test();
            }
        } else if ui
            .add_sized(
                [140.0, 34.0],
                egui::Button::new(egui::RichText::new("■  Cancel").strong()),
            )
            .clicked()
        {
            self.cancel_test();
        }
    }

    fn show_current_speed(&self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Current Speed").size(17.0).strong());

        ui.add_space(10.0);

        if self.samples.is_empty() {
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("0.00 Mbps").size(34.0).strong());

                ui.add_space(4.0);

                ui.label(egui::RichText::new("Waiting for throughput data...").weak());
            });
        } else {
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(Self::format_speed(self.current_mbps))
                        .size(36.0)
                        .strong(),
                );

                ui.add_space(4.0);

                ui.label(egui::RichText::new("Live throughput").weak());

                ui.add_space(10.0);

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
                    .weak(),
                );
            });
        }

        if self.running {
            ui.add_space(12.0);

            let duration = self.duration.parse::<f32>().unwrap_or(1.0);
            let progress = (self.elapsed_seconds as f32 / duration).clamp(0.0, 1.0);

            ui.add(
                egui::ProgressBar::new(progress)
                    .desired_width(260.0)
                    .text(format!("{:.1}s / {}s", self.elapsed_seconds, self.duration)),
            );
        }
    }

    fn show_graph(&self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Live Throughput").size(17.0).strong());

        ui.add_space(6.0);

        if self.samples.is_empty() {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 260.0),
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    ui.label(
                        egui::RichText::new(
                            "No throughput samples yet.\n\n\
                           Start a test to see live network performance.",
                        )
                        .size(15.0)
                        .color(ui.visuals().text_color()),
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

        let line = Line::new("Throughput", points);

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

    fn show_results(&self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Results").size(17.0).strong());

        ui.add_space(8.0);

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

        let sender = self.sender_mbps.map(Self::format_speed).unwrap_or_else(|| {
            if self.samples.is_empty() {
                "—".to_string()
            } else {
                "Calculating...".to_string()
            }
        });

        let receiver = self
            .receiver_mbps
            .map(Self::format_speed)
            .unwrap_or_else(|| {
                if self.samples.is_empty() {
                    "—".to_string()
                } else {
                    "Calculating...".to_string()
                }
            });

        let data = self
            .total_bytes
            .map(Self::format_bytes)
            .unwrap_or_else(|| Self::format_bytes(live_bytes));

        let retransmits = self
            .retransmits
            .map(|value| value.to_string())
            .unwrap_or_else(|| live_retransmits.to_string());

        ui.columns(4, |columns| {
            self.result_card(&mut columns[0], "SENDER", sender);
            self.result_card(&mut columns[1], "RECEIVER", receiver);
            self.result_card(&mut columns[2], "DATA", data);
            self.result_card(&mut columns[3], "RETRANSMITS", retransmits);
        });

        if let Some(error) = &self.error {
            ui.add_space(8.0);

            ui.colored_label(
                egui::Color32::from_rgb(231, 76, 60),
                format!("Error: {}", error),
            );
        }
    }

    fn result_card(&self, ui: &mut egui::Ui, title: &str, value: String) {
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(title)
                            .size(12.0)
                            .strong()
                            .color(ui.visuals().text_color()),
                    );

                    ui.add_space(6.0);

                    ui.label(
                        egui::RichText::new(value)
                            .size(20.0)
                            .strong()
                            .color(ui.visuals().text_color()),
                    );
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

        ui.add_space(12.0);

        self.show_header(ui);

        ui.add_space(12.0);

        ui.separator();

        ui.add_space(12.0);

        ui.columns(2, |columns| {
            egui::Frame::group(columns[0].style())
                .inner_margin(egui::Margin::same(14))
                .show(&mut columns[0], |ui| {
                    self.show_configuration(ui);
                });

            egui::Frame::group(columns[1].style())
                .inner_margin(egui::Margin::same(14))
                .show(&mut columns[1], |ui| {
                    self.show_current_speed(ui);
                });
        });

        ui.add_space(14.0);

        self.show_graph(ui);

        ui.add_space(12.0);

        ui.separator();

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
            cc.egui_ctx.set_visuals(egui::Visuals::light());

            Ok(Box::new(NetworkSpeedApp::default()))
        }),
    )
}
