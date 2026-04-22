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
