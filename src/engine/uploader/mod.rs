//! Qdrant client for batch uploading embeddings
//!
//! This module handles HTTP communication with Qdrant to upload
//! chunks with their embeddings for semantic search.

mod client;
mod file_ops;
mod label_ops;
mod models;
mod search;
#[cfg(test)]
mod tests;
mod upload;

pub use models::{
    is_payload_limit_error, FileSyncInfo, LabelMetadata, PointPayload, PointResult, QdrantId,
    SearchResult,
};
pub use client::{QdrantUploader, DEFAULT_QDRANT_URL};
