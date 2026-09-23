use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::thread;
use std::time::Duration;

use crate::iperf::parser::parse_interval;
use crate::models::{TestEvent, TestSummary};

pub struct IperfConfig {
    pub executable: String,
    pub server: String,
    pub port: u16,
    pub duration_seconds: u32,
}

pub fn detect_iperf3(executable: &str) -> Result<String, String> {
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .map_err(|error| format!("Unable to find iperf3 '{}': {}", executable, error))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);

        return Err(format!("iperf3 version check failed: {}", stderr.trim()));
    }

    let version = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("iperf3 detected")
        .trim()
        .to_string();

    Ok(version)
}

pub fn run_test(config: IperfConfig, sender: Sender<TestEvent>, cancel: Arc<AtomicBool>) {
    let result = run_test_internal(config, &sender, &cancel);

    if let Err(error) = result {
        let _ = sender.send(TestEvent::Error(error));
    }
}

fn run_test_internal(
    config: IperfConfig,
    sender: &Sender<TestEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    let mut child = Command::new(&config.executable)
        .args([
            "-c",
            &config.server,
            "-p",
            &config.port.to_string(),
            "-t",
            &config.duration_seconds.to_string(),
            "-i",
            "1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Failed to start iperf3 '{}': {}", config.executable, error))?;

    let _ = sender.send(TestEvent::Started);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture iperf3 stdout".to_string())?;

    let stdout_sender = sender.clone();

    let stdout_thread = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();

        for line in reader.lines().map_while(Result::ok) {
            if let Some(sample) = parse_interval(&line) {
                let _ = stdout_sender.send(TestEvent::Throughput(sample));
            }

            lines.push(line);
        }

        lines
    });

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture iperf3 stderr".to_string())?;

    let stderr_thread = thread::spawn(move || {
        let reader = BufReader::new(stderr);

        reader
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<String>>()
    });

    // Monitor the process independently so cancellation does not
    // have to wait for the iperf3 duration to finish.
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();

            let _ = stdout_thread.join();
            let _ = stderr_thread.join();

            let _ = sender.send(TestEvent::Cancelled);

            return Ok(());
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout_lines = stdout_thread.join().unwrap_or_default();
                let stderr_lines = stderr_thread.join().unwrap_or_default();

                // Cancellation could happen at almost exactly the same
                // time that iperf3 exits.
                if cancel.load(Ordering::Relaxed) {
                    let _ = sender.send(TestEvent::Cancelled);
                    return Ok(());
                }

                if !status.success() {
                    let details = stderr_lines.join("\n");

                    if details.is_empty() {
                        return Err(format!("iperf3 exited with non-zero status: {}", status));
                    }

                    return Err(format!(
                        "iperf3 exited with non-zero status: {}\n{}",
                        status, details
                    ));
                }

                let mut summary = TestSummary::default();

                for line in stdout_lines {
                    parse_summary_line(&line, &mut summary);
                }

                let _ = sender.send(TestEvent::Finished(summary));

                return Ok(());
            }

            Ok(None) => {
                thread::sleep(Duration::from_millis(50));
            }

            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();

                let _ = stdout_thread.join();
                let _ = stderr_thread.join();

                return Err(format!("Failed to check iperf3 process: {}", error));
            }
        }
    }
}

fn parse_summary_line(line: &str, summary: &mut TestSummary) {
    let parts: Vec<&str> = line.split_whitespace().collect();

    if parts.len() < 8 {
        return;
    }

    if !parts[0].starts_with('[') {
        return;
    }

    let is_sender = parts.contains(&"sender");
    let is_receiver = parts.contains(&"receiver");

    if !is_sender && !is_receiver {
        return;
    }

    let transfer_value = parts.get(4).and_then(|value| value.parse::<f64>().ok());
    let transfer_unit = parts.get(5).copied();

    if let (Some(value), Some(unit)) = (transfer_value, transfer_unit) {
        if let Some(bytes) = bytes_from_unit(value, unit) {
            summary.total_bytes = Some(bytes);
        }
    }

    let bitrate_value = parts.get(6).and_then(|value| value.parse::<f64>().ok());
    let bitrate_unit = parts.get(7).copied();

    if let (Some(value), Some(unit)) = (bitrate_value, bitrate_unit) {
        if let Some(bits_per_second) = bits_per_second_from_unit(value, unit) {
            if is_sender {
                summary.sender_bits_per_second = Some(bits_per_second);
            }

            if is_receiver {
                summary.receiver_bits_per_second = Some(bits_per_second);
            }
        }
    }

    if is_sender {
        summary.retransmits = parts.get(8).and_then(|value| value.parse::<u64>().ok());
    }
}

fn bytes_from_unit(value: f64, unit: &str) -> Option<u64> {
    let multiplier = match unit {
        "Bytes" => 1.0,
        "KBytes" => 1024.0,
        "MBytes" => 1024.0 * 1024.0,
        "GBytes" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };

    Some((value * multiplier) as u64)
}

fn bits_per_second_from_unit(value: f64, unit: &str) -> Option<f64> {
    let multiplier = match unit {
        "bits/sec" => 1.0,
        "Kbits/sec" => 1_000.0,
        "Mbits/sec" => 1_000_000.0,
        "Gbits/sec" => 1_000_000_000.0,
        _ => return None,
    };

    Some(value * multiplier)
}
