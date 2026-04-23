//! Upload operations for Qdrant points.
//!
//! Edit here when: Changing batch upload logic, point building, or size estimation.
//! Do not edit here for: File operations (file_ops.rs), label operations (label_ops.rs), search (search.rs).

use anyhow::{Result, anyhow};

use super::client::QdrantUploader;
use super::models::{Point, PointPayload, UpsertRequest};

impl QdrantUploader {
    /// Uploads a batch of chunks with their embeddings using rewind algorithm.
    ///
    /// Uses the chunk's point_id() for deterministic IDs, enabling
    /// upsert-by-ID semantics for label membership updates.
    ///
    /// # Rewind Algorithm
    ///
    /// If a batch exceeds `max_upload_bytes`, it is split in half and the first half
    /// is uploaded recursively. After successful upload, the algorithm continues with
    /// ALL remaining chunks (not just the other half of the subtree), which may allow
    /// fewer total uploads than pure recursive subdivision.
    ///
    /// # Example
    ///
    /// With 77 chunks (70MB total, 30MB limit):
    /// 1. Serialize 1..77 → 70MB → too big, split
    /// 2. Upload 1..38 recursively (may split further)
    /// 3. Continue with 39..77 → 35MB → too big, split
    /// 4. Upload 39..57 recursively
    /// 5. Continue with 58..77 → uploads directly
    /// 6. Done (potentially 3+ uploads)
    pub fn upload_batch(&self, chunks: &[(crate::engine::Chunk, Vec<f32>)]) -> Result<u64> {
        if chunks.is_empty() {
            return Ok(0);
        }

        let mut remaining: &[(_, _)] = chunks;
        let mut last_operation_id: u64 = 0;
        let mut batch_number: usize = 0;

        while !remaining.is_empty() {
            batch_number += 1;

            // Build points from remaining chunks
            let points = self.build_points(remaining);
            let request_body = UpsertRequest { points };

            // Serialize to check size before sending
            let bytes = serde_json::to_vec(&request_body)?;

            if bytes.len() <= self.max_upload_bytes {
                // Fits within limit - upload directly
                eprintln!(
                    "[Batch {}] Uploading {} chunks ({} bytes / {:.1} MB)",
                    batch_number,
                    remaining.len(),
                    bytes.len(),
                    bytes.len() as f64 / (1024.0 * 1024.0)
                );

                last_operation_id = self.send_upload_batch(&bytes)?;
                break;
            }

            // Batch exceeds limit - need to split
            if remaining.len() == 1 {
                // Fatal: single chunk exceeds limit (should never happen)
                eprintln!();
                eprintln!("═══════════════════════════════════════════════════════════════");
                eprintln!("FATAL: Single chunk exceeds max_upload_bytes limit");
                eprintln!();
                eprintln!(
                    "Chunk size: {} bytes ({:.1} MB)",
                    bytes.len(),
                    bytes.len() as f64 / (1024.0 * 1024.0)
                );
                eprintln!(
                    "Limit: {} bytes ({:.1} MB)",
                    self.max_upload_bytes,
                    self.max_upload_bytes as f64 / (1024.0 * 1024.0)
                );
                eprintln!();
                eprintln!("This is a bug - a single chunk should never exceed the upload limit.");
                eprintln!("Please report this to the maintainers with the following details:");
                eprintln!("  - File that caused this issue");
                eprintln!("  - Approximate size of the file");
                eprintln!("═══════════════════════════════════════════════════════════════");
                return Err(anyhow!(
                    "Single chunk ({} bytes) exceeds max_upload_bytes limit ({} bytes). This is a bug - please report to maintainers.",
                    bytes.len(),
                    self.max_upload_bytes
                ));
            }

            // Rewind: split in half, upload first half recursively, continue with remainder
            let mid = remaining.len() / 2;
            eprintln!(
                "[Batch {}] Batch too large ({} bytes / {:.1} MB > {:.1} MB limit), splitting {} chunks → {} + {}",
                batch_number,
                bytes.len(),
                bytes.len() as f64 / (1024.0 * 1024.0),
                self.max_upload_bytes as f64 / (1024.0 * 1024.0),
                remaining.len(),
                mid,
                remaining.len() - mid
            );

            // Recursively upload first half
            last_operation_id = self.upload_batch(&remaining[..mid])?;

            // Continue loop with remainder
            remaining = &remaining[mid..];
        }

        Ok(last_operation_id)
    }

    /// Build Point structs from chunks (helper for upload_batch)
    fn build_points(&self, chunks: &[(crate::engine::Chunk, Vec<f32>)]) -> Vec<Point> {
        chunks
            .iter()
            .map(|(chunk, embedding)| Point {
                id: chunk.point_id(),
                vector: embedding.clone(),
                payload: PointPayload {
                    text: chunk.text.clone(),
                    source_type: chunk.source_type.clone(),
                    catalog: chunk.catalog.clone(),
                    label_id: chunk.label_id.clone(),
                    active_label_ids: chunk.active_label_ids.clone(),
                    embedder_id: chunk.embedder_id.clone(),
                    chunker_id: chunk.chunker_id.clone(),
                    blob_id: chunk.blob_id.clone(),
                    content_hash: chunk.content_hash.clone(),
                    file_id: chunk.file_id.clone(),
                    relative_path: chunk.relative_path.clone(),
                    package_name: chunk.package_name.clone(),
                    source_uri: chunk.source_uri.clone(),
                    chunk_ordinal: chunk.chunk_ordinal,
                    chunk_count: chunk.chunk_count,
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    symbol_name: chunk.symbol_name.clone(),
                    chunk_type: chunk.chunk_type.clone(),
                    chunk_kind: chunk.chunk_kind.clone(),
                    breadcrumb: Some(chunk.breadcrumb.clone()),
                    split_part_ordinal: chunk.split_part_ordinal,
                    split_part_count: chunk.split_part_count,
                    file_complete: false,
                },
            })
            .collect()
    }

    /// Estimate serialized size of a point without full JSON serialization.
    ///
    /// This uses a cheap heuristic instead of building and serializing the full Point struct.
    /// The estimate does not need to be exact - it's used for accumulation threshold checks.
    pub fn estimate_serialized_size(chunk: &crate::engine::Chunk, embedding: &[f32]) -> usize {
        // Heuristic estimate based on the actual upload structure:
        // - Vector: embedding.len() * 4 bytes (f32) + JSON overhead (~2 chars per number)
        // - Text: chunk.text.len() + JSON string overhead (~2 bytes for quotes)
        // - Other string fields: estimate based on typical lengths
        // - Fixed overhead for JSON structure (~500 bytes)

        // Vector: each f32 becomes a number string, roughly 8-12 chars average
        let vector_size = embedding.len() * 10 + 50; // rough estimate

        // Text field with JSON overhead
        let text_size = chunk.text.len() + 20;

        // String fields (most are small UUIDs or short strings)
        let string_fields_size = chunk.point_id().len()
            + chunk.source_type.len()
            + chunk.catalog.len()
            + chunk.label_id.len()
            + chunk
                .active_label_ids
                .iter()
                .map(|s| s.len() + 4)
                .sum::<usize>()
            + chunk.embedder_id.len()
            + chunk.chunker_id.len()
            + chunk.blob_id.len()
            + chunk.content_hash.len()
            + chunk.file_id.len()
            + chunk.relative_path.len()
            + chunk.package_name.len()
            + chunk.source_uri.len()
            + chunk.symbol_name.as_ref().map(|s| s.len()).unwrap_or(0)
            + chunk.chunk_type.len()
            + chunk.chunk_kind.len()
            + chunk.breadcrumb.len()
            + 200; // JSON string overhead (quotes, colons, etc.)

        // Fixed overhead for JSON structure, numeric fields, and wrapper
        let fixed_overhead = 500;

        vector_size + text_size + string_fields_size + fixed_overhead
    }
}
