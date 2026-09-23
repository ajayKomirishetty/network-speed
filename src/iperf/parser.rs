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

    if parts[1] == "ID]" || parts.contains(&"sender") || parts.contains(&"receiver") {
        return None;
    }

    let interval = parts[2];

    let mut interval_parts = interval.split('-');

    let start_seconds: f64 = interval_parts.next()?.parse().ok()?;
    let end_seconds: f64 = interval_parts.next()?.parse().ok()?;

    let transfer_value: f64 = parts[4].parse().ok()?;
    let transfer_unit = parts[5];

    let transfer_bytes = bytes_from_unit(transfer_value, transfer_unit)?;

    let bitrate_value: f64 = parts[6].parse().ok()?;
    let bitrate_unit = parts[7];

    let bits_per_second = bits_per_second_from_unit(bitrate_value, bitrate_unit)?;

    let retransmits = if parts.len() > 8 {
        parts[8].parse::<u64>().ok()
    } else {
        None
    };

    Some(ThroughputSample {
        start_seconds,
        end_seconds,
        transfer_bytes,
        bits_per_second,
        retransmits,
    })
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
