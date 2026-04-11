use chrono::{DateTime, Utc};

pub fn format_ts(ts: i64) -> String {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}

pub fn human_bytes(b: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if b >= GB      { format!("{:.1} GB", b as f64 / GB as f64) }
    else if b >= MB { format!("{:.1} MB", b as f64 / MB as f64) }
    else if b >= KB { format!("{:.1} KB", b as f64 / KB as f64) }
    else            { format!("{} B", b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_under_kb() { assert_eq!(human_bytes(512), "512 B"); }

    #[test]
    fn bytes_kb() { assert_eq!(human_bytes(2048), "2.0 KB"); }

    #[test]
    fn bytes_mb() { assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB"); }

    #[test]
    fn format_ts_valid() {
        let s = format_ts(0);
        assert!(s.contains("1970"));
    }

    #[test]
    fn format_ts_negative() {
        // negative ts is still valid (pre-epoch) — chrono returns a date
        let s = format_ts(-1);
        assert!(s.contains("1969") || s.contains("UTC"));
    }
}
