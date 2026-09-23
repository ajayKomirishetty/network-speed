use crate::models::{TestSummary, ThroughputSample};
use std::path::Path;

pub struct ExportContext<'a> {
    pub server: &'a str,
    pub port: u16,
    pub duration_seconds: u32,
    pub samples: &'a [ThroughputSample],
    pub summary: &'a TestSummary,
}

fn opt_f64(value: Option<f64>) -> String {
    value
        .map(|v| format!("{:.2}", v))
        .unwrap_or_else(|| "n/a".to_string())
}

fn opt_u64(value: Option<u64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "n/a".to_string())
}

/// Comma-separated values: one row per one-second interval, followed by
/// a summary section.
pub fn to_csv(ctx: &ExportContext) -> String {
    let mut out = String::new();

    out.push_str("interval_start_s,interval_end_s,transfer_bytes,bits_per_second,retransmits\n");

    for sample in ctx.samples {
        out.push_str(&format!(
            "{:.2},{:.2},{},{:.2},{}\n",
            sample.start_seconds,
            sample.end_seconds,
            sample.transfer_bytes,
            sample.bits_per_second,
            sample
                .retransmits
                .map(|v| v.to_string())
                .unwrap_or_else(|| "n/a".to_string()),
        ));
    }

    out.push_str(
        "\nsummary_sender_bps,summary_receiver_bps,sent_bytes,received_bytes,retransmits\n",
    );
    out.push_str(&format!(
        "{},{},{},{},{}\n",
        opt_f64(ctx.summary.sender_bits_per_second),
        opt_f64(ctx.summary.receiver_bits_per_second),
        opt_u64(ctx.summary.sent_bytes),
        opt_u64(ctx.summary.received_bytes),
        opt_u64(ctx.summary.retransmits),
    ));

    out.push_str(&format!(
        "\nserver,port,duration_seconds\n{},{},{}\n",
        ctx.server, ctx.port, ctx.duration_seconds,
    ));

    out
}

/// Machine-readable JSON document with the test configuration,
/// per-interval samples, and the final summary.
pub fn to_json(ctx: &ExportContext) -> Result<String, String> {
    let samples: Vec<serde_json::Value> = ctx
        .samples
        .iter()
        .map(|sample| {
            serde_json::json!({
                "start_seconds": sample.start_seconds,
                "end_seconds": sample.end_seconds,
                "transfer_bytes": sample.transfer_bytes,
                "bits_per_second": sample.bits_per_second,
                "retransmits": sample.retransmits,
            })
        })
        .collect();

    let document = serde_json::json!({
        "tool": "Voyis Network Speed Test",
        "config": {
            "server": ctx.server,
            "port": ctx.port,
            "duration_seconds": ctx.duration_seconds,
            "protocol": "TCP",
        },
        "summary": {
            "sender_bits_per_second": ctx.summary.sender_bits_per_second,
            "receiver_bits_per_second": ctx.summary.receiver_bits_per_second,
            "sent_bytes": ctx.summary.sent_bytes,
            "received_bytes": ctx.summary.received_bytes,
            "retransmits": ctx.summary.retransmits,
        },
        "intervals": samples,
    });

    serde_json::to_string_pretty(&document)
        .map_err(|error| format!("Failed to serialize results: {}", error))
}

/// Human-readable plain-text report.
pub fn to_txt(ctx: &ExportContext) -> String {
    let mut out = String::new();

    out.push_str("Voyis Network Speed Test - Result Report\n");
    out.push_str("========================================\n\n");

    out.push_str(&format!(
        "Server:   {}:{}\nDuration: {} s\nProtocol: TCP\n\n",
        ctx.server, ctx.port, ctx.duration_seconds
    ));

    out.push_str("Summary\n-------\n");
    out.push_str(&format!(
        "Sender throughput:   {} Mbps\n",
        opt_f64(ctx.summary.sender_bits_per_second.map(|v| v / 1_000_000.0))
    ));
    out.push_str(&format!(
        "Receiver throughput: {} Mbps\n",
        opt_f64(
            ctx.summary
                .receiver_bits_per_second
                .map(|v| v / 1_000_000.0)
        )
    ));
    out.push_str(&format!(
        "Data sent:           {} bytes\n",
        opt_u64(ctx.summary.sent_bytes)
    ));
    out.push_str(&format!(
        "Data received:       {} bytes\n",
        opt_u64(ctx.summary.received_bytes)
    ));
    out.push_str(&format!(
        "Retransmits:          {}\n\n",
        opt_u64(ctx.summary.retransmits)
    ));

    out.push_str("Per-interval throughput\n-----------------------\n");
    out.push_str("start_s  end_s    Mbps\n");
    for sample in ctx.samples {
        out.push_str(&format!(
            "{:>7.2} {:>6.2} {:>8.2}\n",
            sample.start_seconds,
            sample.end_seconds,
            sample.bits_per_second / 1_000_000.0,
        ));
    }

    out
}

pub fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents)
        .map_err(|error| format!("Could not write {}: {}", path.display(), error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_context() -> (Vec<ThroughputSample>, TestSummary) {
        let samples = vec![
            ThroughputSample {
                start_seconds: 0.0,
                end_seconds: 1.0,
                transfer_bytes: 1_000_000,
                bits_per_second: 8_000_000.0,
                retransmits: Some(0),
            },
            ThroughputSample {
                start_seconds: 1.0,
                end_seconds: 2.0,
                transfer_bytes: 1_250_000,
                bits_per_second: 10_000_000.0,
                retransmits: Some(2),
            },
        ];

        let summary = TestSummary {
            sender_bits_per_second: Some(9_000_000.0),
            receiver_bits_per_second: Some(8_900_000.0),
            sent_bytes: Some(2_250_000),
            received_bytes: Some(2_240_000),
            retransmits: Some(2),
        };

        (samples, summary)
    }

    #[test]
    fn csv_contains_intervals_and_summary() {
        let (samples, summary) = sample_context();
        let ctx = ExportContext {
            server: "10.0.0.1",
            port: 5201,
            duration_seconds: 2,
            samples: &samples,
            summary: &summary,
        };

        let csv = to_csv(&ctx);

        assert!(csv.contains("interval_start_s,interval_end_s"));
        assert!(csv.contains("0.00,1.00,1000000,8000000.00,0"));
        assert!(csv.contains("1.00,2.00,1250000,10000000.00,2"));
        assert!(csv.contains("summary_sender_bps"));
        assert!(csv.contains("9000000.00,8900000.00,2250000,2240000,2"));
        assert!(csv.contains("10.0.0.1,5201,2"));
    }

    #[test]
    fn json_is_valid_and_complete() {
        let (samples, summary) = sample_context();
        let ctx = ExportContext {
            server: "10.0.0.1",
            port: 5201,
            duration_seconds: 2,
            samples: &samples,
            summary: &summary,
        };

        let json = to_json(&ctx).expect("json export failed");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("export is not valid JSON");

        assert_eq!(parsed["config"]["server"], "10.0.0.1");
        assert_eq!(parsed["config"]["protocol"], "TCP");
        assert_eq!(parsed["summary"]["retransmits"], 2);
        assert_eq!(parsed["intervals"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn txt_report_is_human_readable() {
        let (samples, summary) = sample_context();
        let ctx = ExportContext {
            server: "10.0.0.1",
            port: 5201,
            duration_seconds: 2,
            samples: &samples,
            summary: &summary,
        };

        let txt = to_txt(&ctx);

        assert!(txt.contains("Voyis Network Speed Test"));
        assert!(txt.contains("Sender throughput:   9.00 Mbps"));
        assert!(txt.contains("Receiver throughput: 8.90 Mbps"));
        assert!(txt.contains("Retransmits:          2"));
    }

    #[test]
    fn exports_handle_missing_summary_values() {
        let samples = Vec::new();
        let summary = TestSummary::default();
        let ctx = ExportContext {
            server: "h",
            port: 1,
            duration_seconds: 1,
            samples: &samples,
            summary: &summary,
        };

        let csv = to_csv(&ctx);
        assert!(csv.contains("n/a,n/a,n/a,n/a,n/a"));

        let txt = to_txt(&ctx);
        assert!(txt.contains("n/a"));
    }
}
