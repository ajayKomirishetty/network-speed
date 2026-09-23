use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::thread;

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

    // Read stderr on a separate thread so the stderr pipe
    // cannot fill up and block the iperf3 process.
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture iperf3 stderr".to_string())?;

    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);

        reader
            .lines()
            .filter_map(Result::ok)
            .collect::<Vec<String>>()
    });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture iperf3 stdout".to_string())?;

    let reader = BufReader::new(stdout);

    let mut summary = TestSummary::default();

    for line in reader.lines() {
        // Check whether the user requested cancellation.
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();

            // Make sure the process is actually reaped.
            let _ = child.wait();

            let _ = stderr_handle.join();

            let _ = sender.send(TestEvent::Cancelled);

            return Ok(());
        }

        let line = line.map_err(|error| format!("Failed to read iperf3 output: {}", error))?;

        if let Some(sample) = parse_interval(&line) {
            let _ = sender.send(TestEvent::Throughput(sample));
        }

        parse_summary_line(&line, &mut summary);
    }

    let status = child
        .wait()
        .map_err(|error| format!("Failed to wait for iperf3: {}", error))?;

    let stderr_lines = stderr_handle.join().unwrap_or_default();

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

    let _ = sender.send(TestEvent::Finished(summary));

    Ok(())
}

fn parse_summary_line(line: &str, summary: &mut TestSummary) {
    let parts: Vec<&str> = line.split_whitespace().collect();

    if parts.len() < 7 {
        return;
    }

    if !parts[0].starts_with('[') {
        return;
    }

    if parts.contains(&"sender") || parts.contains(&"receiver") {
        let is_sender = parts.contains(&"sender");
        let is_receiver = parts.contains(&"receiver");

        let transfer_value = parts.get(4).and_then(|v| v.parse::<f64>().ok());

        let transfer_unit = parts.get(5).copied();

        let bitrate_value = parts.get(6).and_then(|v| v.parse::<f64>().ok());

        let bitrate_unit = parts.get(7).copied();

        if let (Some(value), Some(unit)) = (transfer_value, transfer_unit) {
            let bytes = match unit {
                "Bytes" => value,
                "KBytes" => value * 1024.0,
                "MBytes" => value * 1024.0 * 1024.0,
                "GBytes" => value * 1024.0 * 1024.0 * 1024.0,
                _ => return,
            };

            summary.total_bytes = Some(bytes as u64);
        }

        if let (Some(value), Some(unit)) = (bitrate_value, bitrate_unit) {
            let bits_per_second = match unit {
                "bits/sec" => value,
                "Kbits/sec" => value * 1_000.0,
                "Mbits/sec" => value * 1_000_000.0,
                "Gbits/sec" => value * 1_000_000_000.0,
                _ => return,
            };

            if is_sender {
                summary.sender_bits_per_second = Some(bits_per_second);
            }

            if is_receiver {
                summary.receiver_bits_per_second = Some(bits_per_second);
            }
        }

        if is_sender {
            summary.retransmits = parts.get(8).and_then(|v| v.parse().ok());
        }
    }
}
