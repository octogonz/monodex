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
pub fn compute_chunk_id(file: &str, start_line: usize, part: usize) -> u64 {
    let mut hasher = XxHash64::with_seed(0);
    file.hash(&mut hasher);
    start_line.hash(&mut hasher);
    part.hash(&mut hasher);
    hasher.finish()
}

/// Convert u64 chunk ID to display format (8-char lowercase hex with # prefix)
pub fn display_id(hash: u64) -> String {
    format!("#{:08x}", (hash >> 32) & 0xFFFFFFFF)
}
