//! Types for Qdrant API requests, responses, and domain payloads.
//!
//! Edit here when: Adding or modifying Qdrant wire types, payload schemas, or serialization logic.
//! Do not edit here for: HTTP client logic (client.rs), upload operations (upload.rs).

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::engine::identifier::{LabelId, validate_catalog};

/// Check if an error response from Qdrant indicates a payload size limit error.
/// These errors require batch subdivision and are not recoverable with retry.
pub fn is_payload_limit_error(body: &str) -> bool {
    body.contains("Payload error") && body.contains("larger than allowed")
}

/// Custom deserializer for active_label_ids that handles both formats:
/// - Normal array: `["label1", "label2"]`
/// - Qdrant values wrapper: `{"values": ["label1", "label2"]}`
pub(super) fn deserialize_label_ids<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum LabelIdsFormat {
        Array(Vec<String>),
        Values { values: Vec<String> },
    }

    match LabelIdsFormat::deserialize(deserializer) {
        Ok(LabelIdsFormat::Array(arr)) => Ok(arr),
        Ok(LabelIdsFormat::Values { values }) => Ok(values),
        Err(_) => {
            // If deserialization fails, return empty vec (field may be missing)
            // This matches the #[serde(default)] behavior
            Ok(Vec::new())
        }
    }
}

/// Qdrant filter for queries
#[derive(Debug, Serialize)]
pub(super) struct Filter {
    pub must: Vec<Condition>,
}

#[derive(Debug, Serialize)]
pub(super) struct Condition {
    pub key: String,
    pub r#match: MatchValue,
}

#[derive(Debug, Serialize)]
pub(super) struct MatchValue {
    pub value: String,
}

/// Request body for filter-based operations (delete, etc.)
#[derive(Debug, Serialize)]
pub(super) struct FilterRequest {
    pub filter: Filter,
}

/// Request body for Qdrant upsert operation
#[derive(Debug, Serialize)]
pub(super) struct UpsertRequest {
    pub points: Vec<Point>,
}

/// A single point in Qdrant
#[derive(Debug, Serialize)]
pub(super) struct Point {
    pub id: String, // Random UUID
    pub vector: Vec<f32>,
    pub payload: PointPayload,
}

/// Payload associated with a code chunk point
#[derive(Debug, Serialize, Deserialize)]
pub struct PointPayload {
    pub text: String,
    pub source_type: String, // "code"

    // Label membership
    pub catalog: String,
    pub label_id: String, // Transitional: the initiating label. Prefer active_label_ids.
    #[serde(default, deserialize_with = "deserialize_label_ids")]
    pub active_label_ids: Vec<String>, // All labels this chunk belongs to (authoritative)

    // Implementation identity
    pub embedder_id: String, // e.g., "jina-embeddings-v2-base-code:v1"
    pub chunker_id: String,  // e.g., "typescript-partitioner:v1"

    // Provenance
    pub blob_id: String,      // Git blob SHA
    pub content_hash: String, // Hash of chunk text

    // File identity
    pub file_id: String, // Semantic file identity (for grouping chunks)

    // Path context (for retrieval without Git)
    pub relative_path: String,
    pub package_name: String,
    pub source_uri: String, // Useful for locating in Git/GitHub, but NOT a key

    // Chunk metadata
    pub chunk_ordinal: usize, // 1-indexed position in file
    pub chunk_count: usize,
    pub start_line: usize,
    pub end_line: usize,

    // Semantic context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    pub chunk_type: String, // AST node type: function, class, method, etc.
    pub chunk_kind: String, // content, imports, changelog, config, fallback-split, degraded-ast-split
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breadcrumb: Option<String>, // Human-readable: package:File.ts:Symbol

    // Split metadata (for oversized sections split into parts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_part_ordinal: Option<usize>, // Which part (1-indexed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_part_count: Option<usize>, // Total parts

    // Sentinel for incremental crawl
    #[serde(default)]
    pub file_complete: bool, // Only true on chunk_ordinal=1
}

impl PointPayload {
    /// Validates all identifier fields after deserialization from Qdrant.
    ///
    /// This is the boundary where storage-form data enters the application.
    /// Any malformed data from Qdrant is a hard error - we do not log-and-skip.
    pub fn validate(&self) -> Result<()> {
        // Validate catalog
        validate_catalog(&self.catalog).map_err(|e| {
            anyhow!(
                "Invalid catalog in stored payload '{}': {}",
                self.catalog,
                e
            )
        })?;

        // Validate each active_label_id using the canonical storage-form parser
        for label_id in &self.active_label_ids {
            LabelId::parse(label_id).map_err(|e| {
                anyhow!("Invalid label_id in stored payload active_label_ids: {}", e)
            })?;
        }

        // Validate that label_id is present in active_label_ids
        if !self.active_label_ids.contains(&self.label_id) {
            return Err(anyhow!(
                "PointPayload label_id '{}' is not present in active_label_ids {:?}",
                self.label_id,
                self.active_label_ids
            ));
        }

        Ok(())
    }
}

/// Metadata for a label, stored as a special point in the collection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelMetadata {
    pub source_type: String, // "label-metadata"
    pub catalog: String,
    pub label_id: String,    // e.g., "rushstack:main" (internal storage form)
    pub label: String,       // e.g., "main" (bare label name)
    pub commit_oid: String,  // Resolved commit SHA
    pub source_kind: String, // "git-commit"
    #[serde(default)]
    pub crawl_complete: bool,
    pub updated_at_unix_secs: u64,
}

impl LabelMetadata {
    /// Validate all identifier fields in this metadata
    pub fn validate(&self) -> Result<()> {
        // Use LabelId::new as the canonical composition and validation path
        let expected_label_id = LabelId::new(&self.catalog, &self.label)
            .map_err(|e| anyhow!("Invalid identifier in stored metadata: {}", e))?;

        // Check label_id consistency (authoritative key for UUID derivation)
        if self.label_id != expected_label_id.as_str() {
            return Err(anyhow!(
                "Label metadata has inconsistent label_id: expected '{}', got '{}'",
                expected_label_id.as_str(),
                self.label_id
            ));
        }

        Ok(())
    }
}

/// Information about a file for incremental sync
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FileSyncInfo {
    pub content_hash: String,
    pub file_complete: bool,
    pub active_label_ids: Vec<String>,
}

/// Response from Qdrant upsert
#[derive(Debug, Deserialize)]
pub(super) struct UpsertResponse {
    pub result: UpsertResult,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct UpsertResult {
    pub operation_id: u64,
}

/// Response from scroll (list points)
#[derive(Debug, Deserialize)]
pub(super) struct ScrollResponse {
    pub result: ScrollResult,
    #[allow(dead_code)]
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ScrollResult {
    pub points: Vec<ScrollPoint>,
    #[serde(default)]
    pub next_page_offset: Option<QdrantId>,
}

/// Scroll point from Qdrant
#[derive(Debug, Deserialize)]
pub(super) struct ScrollPoint {
    #[allow(dead_code)]
    pub id: QdrantId,
    pub payload: PointPayload,
}

/// Qdrant ID can be either a string (UUID) or integer (custom ID)
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum QdrantId {
    String(String),
    Integer(u64),
}

/// Response from delete
#[derive(Debug, Deserialize)]
pub(super) struct DeleteResponse {
    pub result: DeleteResult,
    #[allow(dead_code)]
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct DeleteResult {
    pub operation_id: u64,
}

/// Response from Qdrant search
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(super) struct SearchResponse {
    pub result: Vec<SearchResult>,
    pub status: String,
}

/// Search result from Qdrant
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct SearchResult {
    pub id: QdrantId,
    pub score: f32,
    pub payload: PointPayload,
}

/// A point retrieved by ID (no score, unlike SearchResult)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PointResult {
    pub id: QdrantId,
    pub payload: PointPayload,
}
