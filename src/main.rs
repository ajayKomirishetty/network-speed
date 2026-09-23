mod iperf;
mod models;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
};
use std::thread;

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

    fn reset_results(&mut self) {
        self.samples.clear();
        self.current_mbps = 0.0;
        self.sender_mbps = None;
        self.receiver_mbps = None;
        self.total_bytes = None;
        self.retransmits = None;
        self.error = None;
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
}

impl eframe::App for NetworkSpeedApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.process_events();

        ui.ctx().request_repaint();

        ui.heading("Network Speed Test");

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("Server:");

            ui.text_edit_singleline(&mut self.server);
        });

        ui.horizontal(|ui| {
            ui.label("Port:");

            ui.text_edit_singleline(&mut self.port);
        });

        ui.horizontal(|ui| {
            ui.label("Duration:");

            ui.text_edit_singleline(&mut self.duration);

            ui.label("seconds");
        });

        ui.horizontal(|ui| {
            ui.label("iperf3 path:");

            ui.text_edit_singleline(&mut self.iperf3_path);
        });
        if let Some(version) = &self.iperf3_version {
            ui.label(format!("Detected: {}", version));
        } else {
            ui.label("iperf3: Not detected");
        }

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            if !self.running {
                if ui.button("Start Test").clicked() {
                    self.start_test();
                }
            } else if ui.button("Cancel").clicked() {
                self.cancel_test();
            }

            ui.label(format!("Status: {}", self.status));
        });

        ui.separator();

        ui.heading("Live Throughput");

        ui.label(format!(
            "Current: {}",
            Self::format_speed(self.current_mbps)
        ));

        if !self.samples.is_empty() {
            let points: PlotPoints = self
                .samples
                .iter()
                .map(|sample| [sample.end_seconds, sample.bits_per_second / 1_000_000.0])
                .collect();

            let line = Line::new("Throughput", points);

            Plot::new("throughput_plot")
                .height(250.0)
                .x_axis_label("Time (seconds)")
                .y_axis_label("Mbps")
                .show(ui, |plot_ui| {
                    plot_ui.line(line);
                });
        }

        ui.separator();

        ui.heading("Summary");

        if let Some(value) = self.sender_mbps {
            ui.label(format!("Sender: {}", Self::format_speed(value)));
        }

        if let Some(value) = self.receiver_mbps {
            ui.label(format!("Receiver: {}", Self::format_speed(value)));
        }

        if let Some(bytes) = self.total_bytes {
            ui.label(format!("Total data: {} bytes", bytes));
        }

        if let Some(retransmits) = self.retransmits {
            ui.label(format!("Retransmits: {}", retransmits));
        }

        if let Some(error) = &self.error {
            ui.separator();

            ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "Voyis Network Speed Test",
        options,
        Box::new(|_cc| Ok(Box::new(NetworkSpeedApp::default()))),
    )
}
