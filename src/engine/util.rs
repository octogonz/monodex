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

/// Compute stable u64 chunk ID from file path, start line, and part number
/// (Legacy - used for Phase 3 hash-based IDs, will be removed after Phase 6 migration)
pub fn compute_chunk_id(file: &str, start_line: usize, part: usize) -> u64 {
    let mut hasher = XxHash64::with_seed(0);
    file.hash(&mut hasher);
    start_line.hash(&mut hasher);
    part.hash(&mut hasher);
    hasher.finish()
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

/// Parse 16-char hex string to u64 file ID
pub fn parse_file_id(s: &str) -> Option<u64> {
    u64::from_str_radix(s, 16).ok()
}

/// Convert u64 chunk ID to display format (8-char lowercase hex with # prefix)
pub fn display_id(hash: u64) -> String {
    format!("#{:08x}", (hash >> 32) & 0xFFFFFFFF)
}
