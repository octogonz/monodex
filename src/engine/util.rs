//! Shared utility functions

use sha2::{Sha256, Digest};
use std::hash::{Hash, Hasher};
use twox_hash::XxHash64;

/// Compute SHA256 hash of content
pub fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    format!("sha256:{:x}", result)
}

/// Compute stable file ID from relative path (Phase 6+ approach)
/// The hash is based on the relative path from catalog base, not full filesystem path.
/// This ensures IDs are stable across different machines/users.
pub fn compute_file_id(relative_path: &str) -> u64 {
    let mut hasher = XxHash64::with_seed(0);
    relative_path.hash(&mut hasher);
    hasher.finish()
}

/// Convert u64 file ID to display format (16-char lowercase hex)
pub fn display_file_id(file_id: u64) -> String {
    format!("{:016x}", file_id)
}
