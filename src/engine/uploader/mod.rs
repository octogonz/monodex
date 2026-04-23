//! Qdrant HTTP client for batch uploading embeddings and managing label-based indexes.
//!
//! Edit here when: Changing the public API of the uploader module or adding new submodules.
//! Do not edit here for: Operation-specific logic (see `upload.rs`, `file_ops.rs`, `label_ops.rs`, `search.rs`), wire types (see `models.rs`), client construction (see `client.rs`).

mod client;
mod file_ops;
mod label_ops;
mod models;
mod search;
#[cfg(test)]
mod tests;
mod upload;

pub use client::QdrantUploader;
pub use models::{
    FileSyncInfo, LabelMetadata, PointPayload, PointResult, QdrantId, SearchResult,
    is_payload_limit_error,
};
