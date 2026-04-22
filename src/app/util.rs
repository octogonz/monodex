//! App-wide utility functions for formatting and display.
//!
//! Purpose: Shared helpers for formatting, timestamps, and terminal output.
//! Edit here when: Adding formatting helpers, timestamp utilities,
//! or terminal output functions.

/// Get current timestamp for logging (HH:MM:SS format)
pub fn chrono_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let h = (now / 3600) % 24;
    let m = (now / 60) % 60;
    let s = now % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Format duration in seconds to human-readable string (e.g., "1h 23m" or "5m 30s")
pub fn format_duration(secs: f64) -> String {
    let total_secs = secs as u64;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let s = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else if mins > 0 {
        format!("{}m {}s", mins, s)
    } else {
        format!("{}s", s)
    }
}

/// Format ETA in seconds to human-readable string
pub fn format_eta(secs: f64) -> String {
    if secs <= 0.0 || !secs.is_finite() {
        return "--".to_string();
    }
    format_duration(secs)
}

/// E.1: Sanitize a string for safe terminal output by stripping control characters.
/// This prevents terminal injection attacks from malicious file paths, breadcrumbs, etc.
pub fn sanitize_for_terminal(s: &str) -> String {
    s.chars()
        .filter(|c| {
            // Allow printable ASCII and common Unicode, but strip control characters
            // Control characters are those with code points < 0x20 (space) and DEL (0x7F)
            // Also strip ANSI escape sequences which start with ESC (0x1B)
            !c.is_control() || *c == '\t' || *c == '\n' || *c == '\r'
        })
        .collect()
}
