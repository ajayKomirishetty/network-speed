use crate::models::ThroughputSample;

pub fn parse_interval(line: &str) -> Option<ThroughputSample> {
    let line = line.trim();

    if !line.starts_with('[') {
        return None;
    }

    let parts: Vec<&str> = line.split_whitespace().collect();

    if parts.len() < 8 {
        return None;
    }

    // Ignore headers and final sender/receiver summary lines.
    if parts[1] == "ID]" || parts.contains(&"sender") || parts.contains(&"receiver") {
        return None;
    }

    let interval = parts[2];
    let (start_seconds, end_seconds) = parse_interval_range(interval)?;

    let transfer_value = parts[4].parse::<f64>().ok()?;
    let transfer_bytes = bytes_from_unit(transfer_value, parts[5])?;

    let bitrate_value = parts[6].parse::<f64>().ok()?;
    let bits_per_second = bits_per_second_from_unit(bitrate_value, parts[7])?;

    let retransmits = parts.get(8).and_then(|value| value.parse::<u64>().ok());

    Some(ThroughputSample {
        start_seconds,
        end_seconds,
        transfer_bytes,
        bits_per_second,
        retransmits,
    })
}

fn parse_interval_range(value: &str) -> Option<(f64, f64)> {
    let mut parts = value.split('-');

    let start = parts.next()?.parse::<f64>().ok()?;
    let end = parts.next()?.parse::<f64>().ok()?;

    if parts.next().is_some() {
        return None;
    }

    Some((start, end))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gigabit_interval() {
        let line = "[  5]   0.00-1.01   sec  18.3 GBytes   157 Gbits/sec    0";

        let sample = parse_interval(line).expect("expected interval to parse");

        assert!((sample.start_seconds - 0.00).abs() < f64::EPSILON);
        assert!((sample.end_seconds - 1.01).abs() < f64::EPSILON);

        assert_eq!(
            sample.transfer_bytes,
            (18.3 * 1024.0 * 1024.0 * 1024.0) as u64
        );

        assert_eq!(sample.bits_per_second, 157_000_000_000.0);
        assert_eq!(sample.retransmits, Some(0));
    }

    #[test]
    fn parses_megabit_interval() {
        let line = "[  5]   0.00-1.00   sec  100 MBytes   850 Mbits/sec    2";

        let sample = parse_interval(line).expect("expected interval to parse");

        assert_eq!(sample.bits_per_second, 850_000_000.0);
        assert_eq!(sample.retransmits, Some(2));
    }

    #[test]
    fn parses_kilobit_interval() {
        let line = "[  5]   0.00-1.00   sec  512 KBytes   512 Kbits/sec    0";

        let sample = parse_interval(line).expect("expected interval to parse");

        assert_eq!(sample.bits_per_second, 512_000.0);
        assert_eq!(sample.transfer_bytes, 512 * 1024);
    }

    #[test]
    fn parses_bits_per_second() {
        let line = "[  5]   0.00-1.00   sec  1000 Bytes   800 bits/sec    0";

        let sample = parse_interval(line).expect("expected interval to parse");

        assert_eq!(sample.transfer_bytes, 1000);
        assert_eq!(sample.bits_per_second, 800.0);
    }

    #[test]
    fn ignores_header() {
        let line = "[ ID] Interval           Transfer     Bitrate         Retr";

        assert!(parse_interval(line).is_none());
    }

    #[test]
    fn ignores_sender_summary() {
        let line = "[  5]   0.00-10.00 sec   189 GBytes   162 Gbits/sec   0 sender";

        assert!(parse_interval(line).is_none());
    }

    #[test]
    fn ignores_receiver_summary() {
        let line = "[  5]   0.00-10.00 sec   189 GBytes   162 Gbits/sec receiver";

        assert!(parse_interval(line).is_none());
    }

    #[test]
    fn ignores_non_iperf_line() {
        assert!(parse_interval("iperf Done.").is_none());
        assert!(parse_interval("").is_none());
        assert!(parse_interval("random text").is_none());
    }

    #[test]
    fn ignores_unknown_units() {
        let line = "[  5]   0.00-1.00   sec  100 FooBytes   100 FooBits/sec    0";

        assert!(parse_interval(line).is_none());
    }
}
