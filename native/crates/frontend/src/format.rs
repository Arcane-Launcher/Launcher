//! Formatting helpers — direct port of `src/lib/format.ts`, byte-for-byte
//! identical output for the same input.

/// Total accumulated duration, e.g. "42h 12m played" / "3m played".
pub fn format_playtime(total_seconds: u64) -> String {
    if total_seconds < 60 {
        return "Never played".to_string();
    }
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes}m played")
    } else {
        format!("{minutes}m played")
    }
}

/// A live "hh:mm:ss" elapsed clock, for a currently-running session.
pub fn format_elapsed(started_at_seconds: u64, now_seconds: u64) -> String {
    let total = now_seconds.saturating_sub(started_at_seconds);
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub fn time_ago(unix_seconds: Option<u64>) -> String {
    let Some(unix_seconds) = unix_seconds else {
        return String::new();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let seconds = now.saturating_sub(unix_seconds);
    if seconds < 60 {
        return "just now".to_string();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

pub fn format_bytes(n: u64) -> String {
    if n == 0 {
        return "0 B".to_string();
    }
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    // Math.log(n) / Math.log(1024), clamped to the last unit.
    let i = ((n as f64).ln() / 1024f64.ln()).floor() as usize;
    let i = i.min(UNITS.len() - 1);
    let value = n as f64 / 1024f64.powi(i as i32);
    let decimals = if i == 0 { 0 } else { 1 };
    format!("{value:.decimals$} {}", UNITS[i])
}

pub fn format_speed(bytes_per_second: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_second))
}

pub fn format_eta(remaining_bytes: u64, bytes_per_second: u64) -> Option<String> {
    if bytes_per_second == 0 || remaining_bytes == 0 {
        return None;
    }
    // JS rounds at every step (Math.round), so mirror that exactly.
    let seconds = (remaining_bytes as f64 / bytes_per_second as f64).round() as u64;
    if seconds < 60 {
        return Some(format!("{seconds}s left"));
    }
    let minutes = (seconds as f64 / 60.0).round() as u64;
    if minutes < 60 {
        return Some(format!("{minutes}m left"));
    }
    let hours = (minutes as f64 / 60.0).round() as u64;
    Some(format!("{hours}h left"))
}
