use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::iperf::parser::{bits_per_second_from_unit, bytes_from_unit, parse_interval};
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
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "iperf3 was not found at '{}'. Install iperf3 or set the correct path.",
                    executable
                )
            } else {
                format!("Unable to run iperf3 '{}': {}", executable, error)
            }
        })?;

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

/// Translate raw iperf3 stderr text into a message that helps a
/// non-technical user understand what went wrong and what to try next.
pub fn friendly_error(raw: &str, server: &str, port: u16) -> String {
    let lower = raw.to_lowercase();

    // Check the specific causes before the generic "unable to connect"
    // prefix that iperf3 uses for all of these.
    if lower.contains("name or service not known")
        || lower.contains("temporary failure in name resolution")
        || lower.contains("nodename nor servname")
    {
        format!(
            "Could not resolve the server name \"{server}\".\n\
             Check the spelling or use an IP address instead.\n\n\
             Details from iperf3:\n{raw}"
        )
    } else if lower.contains("no route to host") || lower.contains("network is unreachable") {
        format!(
            "The network path to {server} is unreachable.\n\
             Check that the host address is correct and that your machine has a \
             route to it (VPN, correct network, etc.).\n\n\
             Details from iperf3:\n{raw}"
        )
    } else if lower.contains("unable to connect to server") || lower.contains("connection refused")
    {
        format!(
            "Could not connect to {server}:{port}.\n\
             Make sure an iperf3 server is running on that host and port, \
             and that no firewall is blocking the connection.\n\n\
             Details from iperf3:\n{raw}"
        )
    } else if lower.contains("control socket has closed") || lower.contains("connection reset") {
        format!(
            "The iperf3 server closed the connection unexpectedly.\n\
             The server may have been stopped or rejected the test parameters.\n\n\
             Details from iperf3:\n{raw}"
        )
    } else {
        raw.to_string()
    }
}

pub fn run_test(config: IperfConfig, sender: Sender<TestEvent>, cancel: Arc<AtomicBool>) {
    let result = run_test_internal(config, &sender, &cancel, DEFAULT_TIMEOUTS);

    if let Err(error) = result {
        let _ = sender.send(TestEvent::Error(error));
    }
}

/// Bounds for a single run.
///
/// * `connect`: how long to wait for iperf3's control connection before
///   giving up with a "could not connect" error.
/// * `hung_grace`: extra time past the requested test duration before a
///   connected-but-never-exiting child is treated as hung and killed.
struct RunTimeouts {
    connect: Duration,
    hung_grace: Duration,
}

const DEFAULT_TIMEOUTS: RunTimeouts = RunTimeouts {
    connect: Duration::from_secs(10),
    hung_grace: Duration::from_secs(15),
};

fn run_test_internal(
    config: IperfConfig,
    sender: &Sender<TestEvent>,
    cancel: &Arc<AtomicBool>,
    timeouts: RunTimeouts,
) -> Result<(), String> {
    let mut child = Command::new(&config.executable)
        .args([
            "-c",
            &config.server,
            "-p",
            &config.port.to_string(),
            "-t",
            &config.duration_seconds.to_string(),
            // Report once per second, and flush stdout after every
            // interval so results stream live through the pipe instead
            // of arriving buffered at the end.
            "-i",
            "1",
            "--forceflush",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "iperf3 was not found at '{}'. Install iperf3 or set the correct path.",
                    config.executable
                )
            } else {
                format!("Failed to start iperf3 '{}': {}", config.executable, error)
            }
        })?;

    let _ = sender.send(TestEvent::Started);

    let started_at = Instant::now();
    // Give up on the control connection quickly: a server that never
    // answers should produce a precise "could not connect" error, not a
    // generic timeout much later.
    let connect_deadline = started_at + timeouts.connect;
    // Backstop for a run that connects but then never finishes (server
    // wedges mid-test, packets dropped with no RST, ...).
    let finish_deadline =
        started_at + Duration::from_secs(u64::from(config.duration_seconds)) + timeouts.hung_grace;

    // Set when iperf3 reports its control connection ("connected to" on
    // stdout). Until then the run is still in the connect phase.
    let connected = Arc::new(AtomicBool::new(false));
    let connected_flag = Arc::clone(&connected);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture iperf3 stdout".to_string())?;

    let stdout_sender = sender.clone();

    let stdout_thread = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();

        for line in reader.lines().map_while(Result::ok) {
            if line.contains("connected to") {
                connected_flag.store(true, Ordering::Relaxed);
            }

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

        let now = Instant::now();

        // Still no control connection after the connect timeout: the
        // server isn't answering. Report it precisely instead of waiting
        // for the generic finish deadline.
        if !connected.load(Ordering::Relaxed) && now >= connect_deadline {
            let _ = child.kill();
            let _ = child.wait();

            let _ = stdout_thread.join();
            let _ = stderr_thread.join();

            return Err(format!(
                "Could not connect to {}:{} within {} seconds.\n\
                 Make sure an iperf3 server is running on that host and port, \
                 and that no firewall is blocking the connection.",
                config.server,
                config.port,
                timeouts.connect.as_secs()
            ));
        }

        // Connected, but iperf3 should have exited by now: treat it as
        // hung, kill it, and report a timeout instead of leaving the run
        // stuck.
        if now >= finish_deadline {
            let _ = child.kill();
            let _ = child.wait();

            let _ = stdout_thread.join();
            let _ = stderr_thread.join();

            return Err(format!(
                "The test timed out: iperf3 did not finish within {} seconds. \
                 The server may have stopped responding mid-test. \
                 Check that an iperf3 server is healthy at {}:{} and try again.",
                u64::from(config.duration_seconds) + timeouts.hung_grace.as_secs(),
                config.server,
                config.port
            ));
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

                    return Err(friendly_error(&details, &config.server, config.port));
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

    if let (Some(value), Some(unit)) = (transfer_value, transfer_unit)
        && let Some(bytes) = bytes_from_unit(value, unit)
    {
        if is_sender {
            summary.sent_bytes = Some(bytes);
        } else {
            summary.received_bytes = Some(bytes);
        }
    }

    let bitrate_value = parts.get(6).and_then(|value| value.parse::<f64>().ok());
    let bitrate_unit = parts.get(7).copied();

    if let (Some(value), Some(unit)) = (bitrate_value, bitrate_unit)
        && let Some(bits_per_second) = bits_per_second_from_unit(value, unit)
    {
        if is_sender {
            summary.sender_bits_per_second = Some(bits_per_second);
        }

        if is_receiver {
            summary.receiver_bits_per_second = Some(bits_per_second);
        }
    }

    if is_sender {
        summary.retransmits = parts.get(8).and_then(|value| value.parse::<u64>().ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iperf::resolve_iperf3;
    use std::process::Child;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn parses_sender_summary_line() {
        let mut summary = TestSummary::default();

        parse_summary_line(
            "[  5]   0.00-10.00  sec  11.0 MBytes  9.25 Mbits/sec    0             sender",
            &mut summary,
        );

        assert_eq!(summary.sender_bits_per_second, Some(9_250_000.0));
        assert_eq!(summary.sent_bytes, Some(11 * 1024 * 1024));
        assert_eq!(summary.retransmits, Some(0));
        assert_eq!(summary.receiver_bits_per_second, None);
        assert_eq!(summary.received_bytes, None);
    }

    #[test]
    fn parses_receiver_summary_line() {
        let mut summary = TestSummary::default();

        parse_summary_line(
            "[  5]   0.00-10.00  sec  10.8 MBytes  9.03 Mbits/sec                  receiver",
            &mut summary,
        );

        assert_eq!(summary.receiver_bits_per_second, Some(9_030_000.0));
        assert_eq!(
            summary.received_bytes,
            Some((10.8 * 1024.0 * 1024.0) as u64)
        );
        assert_eq!(summary.sender_bits_per_second, None);
        assert_eq!(summary.retransmits, None);
    }

    #[test]
    fn ignores_non_summary_lines() {
        let mut summary = TestSummary::default();

        parse_summary_line(
            "[ ID] Interval           Transfer     Bitrate         Retr",
            &mut summary,
        );
        parse_summary_line("iperf Done.", &mut summary);
        parse_summary_line("", &mut summary);

        assert_eq!(summary.sender_bits_per_second, None);
        assert_eq!(summary.receiver_bits_per_second, None);
    }

    #[test]
    fn friendly_error_maps_connection_refused() {
        let message = friendly_error(
            "iperf3: error - unable to connect to server: Connection refused",
            "example.com",
            5201,
        );

        assert!(message.contains("Could not connect to example.com:5201"));
        assert!(message.contains("iperf3 server is running"));
    }

    #[test]
    fn friendly_error_maps_dns_failure() {
        let message = friendly_error(
            "iperf3: error - unable to connect to server: Name or service not known",
            "no-such-host",
            5201,
        );

        assert!(message.contains("Could not resolve"));
        assert!(message.contains("no-such-host"));
    }

    #[test]
    fn friendly_error_passes_through_unknown_errors() {
        let raw = "iperf3: error - some exotic failure";

        assert_eq!(friendly_error(raw, "h", 1), raw);
    }

    #[test]
    fn resolve_prefers_existing_custom_path() {
        let dir = std::env::temp_dir();
        let fake = dir.join("voyis-test-iperf3-binary");

        std::fs::write(&fake, b"fake").unwrap();

        let resolved = resolve_iperf3(Some(fake.to_string_lossy().as_ref()));

        assert_eq!(resolved, Some(fake.clone()));

        std::fs::remove_file(&fake).ok();
    }

    #[test]
    fn resolve_rejects_missing_custom_path() {
        let resolved = resolve_iperf3(Some("/definitely/not/here/iperf3"));

        // Falls through to bundled copy / PATH; must not return the
        // missing custom path itself.
        assert_ne!(
            resolved,
            Some(std::path::PathBuf::from("/definitely/not/here/iperf3"))
        );
    }

    // --- Integration tests against a real iperf3 binary --------------------
    // These are skipped with a notice when iperf3 is not installed.

    fn iperf3_binary() -> Option<String> {
        resolve_iperf3(None).map(|path| path.to_string_lossy().to_string())
    }

    fn spawn_server(binary: &str, port: u16) -> Option<Child> {
        Command::new(binary)
            .args(["-s", "-p", &port.to_string(), "-1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    }

    fn collect_events(receiver: &mpsc::Receiver<TestEvent>, timeout: Duration) -> Vec<TestEvent> {
        let deadline = Instant::now() + timeout;
        let mut events = Vec::new();

        while Instant::now() < deadline {
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => {
                    let terminal = matches!(
                        event,
                        TestEvent::Finished(_) | TestEvent::Error(_) | TestEvent::Cancelled
                    );
                    events.push(event);
                    if terminal {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
            }
        }

        events
    }

    #[test]
    fn live_run_reports_intervals_and_summary() {
        let Some(binary) = iperf3_binary() else {
            eprintln!("skipping: iperf3 not installed");
            return;
        };

        let port = 52991;
        let mut server = spawn_server(&binary, port).expect("failed to start iperf3 server");
        thread::sleep(Duration::from_millis(500));

        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let config = IperfConfig {
            executable: binary,
            server: "127.0.0.1".to_string(),
            port,
            duration_seconds: 3,
        };

        thread::spawn(move || run_test(config, sender, cancel));

        let events = collect_events(&receiver, Duration::from_secs(20));
        let _ = server.wait();

        let intervals = events
            .iter()
            .filter(|event| matches!(event, TestEvent::Throughput(_)))
            .count();

        assert!(
            intervals >= 2,
            "expected live interval events, got {intervals}"
        );

        let summary = events.iter().find_map(|event| match event {
            TestEvent::Finished(summary) => Some(summary),
            _ => None,
        });

        let summary = summary.expect("expected Finished event");

        assert!(
            summary.sender_bits_per_second.unwrap_or(0.0) > 0.0,
            "expected positive sender bitrate"
        );
        assert!(
            summary.receiver_bits_per_second.unwrap_or(0.0) > 0.0,
            "expected positive receiver bitrate"
        );
        assert!(
            summary.sent_bytes.unwrap_or(0) > 0,
            "expected positive sent bytes"
        );
    }

    #[test]
    fn cancel_stops_test_and_leaves_no_process() {
        let Some(binary) = iperf3_binary() else {
            eprintln!("skipping: iperf3 not installed");
            return;
        };

        let port = 52992;
        let mut server = spawn_server(&binary, port).expect("failed to start iperf3 server");
        thread::sleep(Duration::from_millis(500));

        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);

        let config = IperfConfig {
            executable: binary.clone(),
            server: "127.0.0.1".to_string(),
            port,
            duration_seconds: 60,
        };

        thread::spawn(move || run_test(config, sender, worker_cancel));

        // Wait for the first live sample, then cancel.
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut saw_sample = false;
        while Instant::now() < deadline {
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(TestEvent::Throughput(_)) => {
                    saw_sample = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        assert!(
            saw_sample,
            "expected at least one live sample before cancelling"
        );

        cancel.store(true, Ordering::Relaxed);

        let events = collect_events(&receiver, Duration::from_secs(15));
        let _ = server.wait();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, TestEvent::Cancelled)),
            "expected Cancelled event"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, TestEvent::Finished(_))),
            "cancelled test must not report Finished"
        );

        // No iperf3 client process may be left running.
        thread::sleep(Duration::from_millis(500));
        let leftover = Command::new("pgrep")
            .args(["-f", &format!("iperf3.*-p {port}")])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);

        assert!(!leftover, "iperf3 client process left running after cancel");
    }

    #[test]
    fn refused_connection_produces_friendly_error() {
        let Some(binary) = iperf3_binary() else {
            eprintln!("skipping: iperf3 not installed");
            return;
        };

        // Nothing listens on this port.
        let port = 52993;

        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let config = IperfConfig {
            executable: binary,
            server: "127.0.0.1".to_string(),
            port,
            duration_seconds: 3,
        };

        thread::spawn(move || run_test(config, sender, cancel));

        let events = collect_events(&receiver, Duration::from_secs(20));

        let error = events.iter().find_map(|event| match event {
            TestEvent::Error(message) => Some(message),
            _ => None,
        });

        let error = error.expect("expected Error event");

        assert!(
            error.contains("Could not connect to 127.0.0.1:52993"),
            "expected friendly connection error, got: {error}"
        );
    }

    /// A server that never answers the control connection must fail fast
    /// with a "could not connect" error, not hang until the generic
    /// finish deadline.
    #[cfg(unix)]
    #[test]
    fn unanswered_server_fails_fast_with_connect_error() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("voyis-conn-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("silent-iperf3");
        let mut script = std::fs::File::create(&fake).unwrap();
        // Never prints "connected to": simulates a server that answers
        // nothing and never fails on its own either.
        writeln!(script, "#!/bin/sh").unwrap();
        writeln!(script, "exec sleep 60").unwrap();
        drop(script);
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (sender, _receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let config = IperfConfig {
            executable: fake.to_string_lossy().to_string(),
            server: "127.0.0.6".to_string(),
            port: 5201,
            duration_seconds: 10,
        };

        let timeouts = RunTimeouts {
            connect: Duration::from_secs(2),
            hung_grace: Duration::from_secs(60),
        };

        let started = Instant::now();
        let result = run_test_internal(config, &sender, &cancel, timeouts);
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(15),
            "unanswered server must fail at the connect timeout, took {elapsed:?}"
        );

        let error = result.expect_err("expected a connect error");
        assert!(
            error.contains("Could not connect to 127.0.0.6:5201"),
            "expected connect error, got: {error}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A run that connects but then never finishes must be killed at the
    /// finish deadline and reported as a timeout.
    #[cfg(unix)]
    #[test]
    fn hung_connected_test_is_killed_and_reports_timeout() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("voyis-hang-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("hang-iperf3");
        let mut script = std::fs::File::create(&fake).unwrap();
        // Report the control connection, then hang forever: simulates a
        // server that wedges mid-test.
        writeln!(script, "#!/bin/sh").unwrap();
        writeln!(
            script,
            "echo '[  5] local 127.0.0.1 port 54321 connected to 127.0.0.1 port 5201'"
        )
        .unwrap();
        writeln!(script, "exec sleep 60").unwrap();
        drop(script);
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (sender, _receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let config = IperfConfig {
            executable: fake.to_string_lossy().to_string(),
            server: "127.0.0.1".to_string(),
            port: 5201,
            duration_seconds: 1,
        };

        let timeouts = RunTimeouts {
            connect: Duration::from_secs(5),
            hung_grace: Duration::from_secs(2),
        };

        let started = Instant::now();
        let result = run_test_internal(config, &sender, &cancel, timeouts);
        let elapsed = started.elapsed();

        // 1s duration + 2s grace: the call must return well before the
        // 60s sleeper would exit on its own.
        assert!(
            elapsed < Duration::from_secs(15),
            "hung child must be killed promptly, took {elapsed:?}"
        );

        let error = result.expect_err("expected a timeout error");
        assert!(
            error.contains("timed out"),
            "expected timeout message, got: {error}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
