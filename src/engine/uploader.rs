//! Qdrant client for batch uploading embeddings
//!
//! This module handles HTTP communication with Qdrant to upload
//! chunks with their embeddings for semantic search.

use anyhow::{Result, anyhow};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

const DEFAULT_QDRANT_URL: &str = "http://localhost:6333";

/// Check if an error response from Qdrant indicates a payload size limit error.
/// These errors require batch subdivision and are not recoverable with retry.
pub fn is_payload_limit_error(body: &str) -> bool {
    body.contains("Payload error") && body.contains("larger than allowed")
}

/// Custom deserializer for active_label_ids that handles both formats:
/// - Normal array: `["label1", "label2"]`
/// - Qdrant values wrapper: `{"values": ["label1", "label2"]}`
fn deserialize_label_ids<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
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
struct Filter {
    must: Vec<Condition>,
}

#[derive(Debug, Serialize)]
struct Condition {
    key: String,
    r#match: MatchValue,
}

#[derive(Debug, Serialize)]
struct MatchValue {
    value: String,
}

/// Request body for filter-based operations (delete, etc.)
#[derive(Debug, Serialize)]
struct FilterRequest {
    filter: Filter,
}

/// Qdrant client for uploading embeddings
pub struct QdrantUploader {
    client: Client,
    url: String,
    collection: String,
    debug: bool,
}

/// Request body for Qdrant upsert operation
#[derive(Debug, Serialize)]
struct UpsertRequest {
    points: Vec<Point>,
}

/// A single point in Qdrant
#[derive(Debug, Serialize)]
struct Point {
    id: String, // Random UUID
    vector: Vec<f32>,
    payload: PointPayload,
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
    pub chunk_kind: String, // content, imports, changelog, config
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breadcrumb: Option<String>, // Human-readable: package:File.ts:Symbol

    // Sentinel for incremental crawl
    #[serde(default)]
    pub file_complete: bool, // Only true on chunk_ordinal=1
}

/// Metadata for a label, stored as a special point in the collection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelMetadata {
    pub source_type: String, // "label-metadata"
    pub catalog: String,
    pub label_id: String,    // e.g., "rushstack:main"
    pub label_name: String,  // e.g., "main"
    pub commit_oid: String,  // Resolved commit SHA
    pub source_kind: String, // "git-commit"
    #[serde(default)]
    pub crawl_complete: bool,
    pub updated_at_unix_secs: u64,
}

/// Information about a file for incremental sync
/// Note: Fields are currently unused but kept for future incremental sync implementation
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FileSyncInfo {
    pub content_hash: String,
    pub file_complete: bool,
}

/// Response from Qdrant upsert
#[derive(Debug, Deserialize)]
struct UpsertResponse {
    result: UpsertResult,
    status: String,
}

#[derive(Debug, Deserialize)]
struct UpsertResult {
    operation_id: u64,
}

/// Response from scroll (list points)
#[derive(Debug, Deserialize)]
struct ScrollResponse {
    result: ScrollResult,
    #[allow(dead_code)]
    status: String,
}

#[derive(Debug, Deserialize)]
struct ScrollResult {
    points: Vec<ScrollPoint>,
    #[serde(default)]
    next_page_offset: Option<QdrantId>,
}

/// Scroll point from Qdrant
#[derive(Debug, Deserialize)]
struct ScrollPoint {
    #[allow(dead_code)]
    id: QdrantId,
    payload: PointPayload,
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
struct DeleteResponse {
    result: DeleteResult,
    #[allow(dead_code)]
    status: String,
}

#[derive(Debug, Deserialize)]
struct DeleteResult {
    operation_id: u64,
}

/// Response from Qdrant search
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SearchResponse {
    result: Vec<SearchResult>,
    status: String,
}

/// Search result from Qdrant
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct SearchResult {
    pub id: QdrantId,
    pub score: f32,
    pub payload: PointPayload,
}

impl QdrantUploader {
    /// Creates a new Qdrant uploader
    pub fn new(collection: &str, qdrant_url: Option<&str>, debug: bool) -> Result<Self> {
        let url = qdrant_url.unwrap_or(DEFAULT_QDRANT_URL).to_string();

        // Use a longer timeout to accommodate wait=true operations
        // which require Qdrant to fully index points before responding
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300)) // 5 minutes
            .build()?;

        Ok(Self {
            client,
            url,
            collection: collection.to_string(),
            debug,
        })
    }

    /// Delete all points for a specific catalog
    #[allow(dead_code)]
    pub fn delete_catalog(&self, catalog: &str) -> Result<u64> {
        let endpoint = format!("{}/collections/{}/points/delete", self.url, self.collection);

        let request_body = FilterRequest {
            filter: Filter {
                must: vec![Condition {
                    key: "catalog".to_string(),
                    r#match: MatchValue {
                        value: catalog.to_string(),
                    },
                }],
            },
        };

        let response = self.client.post(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to delete catalog: HTTP {}",
                response.status()
            ));
        }

        let delete_response: DeleteResponse = response.json()?;
        Ok(delete_response.result.operation_id)
    }

    /// Delete all points for a specific file
    /// Legacy - uses source_uri filter, not the file-id-centric model
    #[allow(dead_code)]
    pub fn delete_file(&self, file_path: &str, catalog: &str) -> Result<u64> {
        let endpoint = format!("{}/collections/{}/points/delete", self.url, self.collection);

        let request_body = FilterRequest {
            filter: Filter {
                must: vec![
                    Condition {
                        key: "catalog".to_string(),
                        r#match: MatchValue {
                            value: catalog.to_string(),
                        },
                    },
                    Condition {
                        key: "source_uri".to_string(),
                        r#match: MatchValue {
                            value: file_path.to_string(),
                        },
                    },
                ],
            },
        };

        let response = self.client.post(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Err(anyhow!("Failed to delete file: HTTP {}", response.status()));
        }

        let delete_response: DeleteResponse = response.json()?;
        Ok(delete_response.result.operation_id)
    }

    /// Get all points for a specific catalog
    /// Returns a map of file path → FileSyncInfo (content_hash and file_complete)
    /// Only queries chunk #1 per file for efficiency
    #[allow(dead_code)]
    pub fn get_catalog_files(
        &self,
        catalog: &str,
    ) -> Result<std::collections::HashMap<String, FileSyncInfo>> {
        let mut files = std::collections::HashMap::new();
        let mut offset: Option<QdrantId> = None;
        const LIMIT: u32 = 1000;

        loop {
            let endpoint = format!(
                "{}/collections/{}/points/scroll?limit={}",
                self.url, self.collection, LIMIT
            );

            // Build filter with catalog AND chunk_ordinal=1
            #[derive(Debug, Serialize)]
            struct ScrollRequestWithIntFilter {
                filter: FilterWithIntCondition,
                with_payload: bool,
                #[serde(skip_serializing_if = "Option::is_none")]
                offset: Option<QdrantId>,
            }

            #[derive(Debug, Serialize)]
            struct FilterWithIntCondition {
                must: Vec<serde_json::Value>,
            }

            let must_values = vec![
                serde_json::json!({
                    "key": "catalog",
                    "match": { "value": catalog }
                }),
                serde_json::json!({
                    "key": "chunk_ordinal",
                    "match": { "value": 1 }
                }),
                serde_json::json!({
                    "key": "source_type",
                    "match": { "value": "code" }
                }),
            ];

            let request_body = ScrollRequestWithIntFilter {
                filter: FilterWithIntCondition { must: must_values },
                with_payload: true,
                offset: offset.clone(),
            };

            let response = self.client.post(&endpoint).json(&request_body).send()?;

            if !response.status().is_success() {
                return Err(anyhow!(
                    "Failed to scroll catalog: HTTP {}",
                    response.status()
                ));
            }

            let response_text = response.text()?;
            let scroll_response: ScrollResponse = match serde_json::from_str(&response_text) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Failed to deserialize Qdrant response: {}", e);
                    eprintln!(
                        "Raw response (first 2000 chars): {}",
                        &response_text.chars().take(2000).collect::<String>()
                    );
                    return Err(anyhow!("Deserialization error: {}", e));
                }
            };

            if scroll_response.result.points.is_empty() {
                break;
            }

            for point in scroll_response.result.points {
                files.insert(
                    point.payload.source_uri.clone(),
                    FileSyncInfo {
                        content_hash: point.payload.content_hash.clone(),
                        file_complete: point.payload.file_complete,
                    },
                );
            }

            offset = scroll_response.result.next_page_offset;
            if offset.is_none() {
                break;
            }
        }

        Ok(files)
    }

    /// Uploads a batch of chunks with their embeddings
    /// Uploads a batch of chunks with their embeddings
    ///
    /// Uses the chunk's point_id() for deterministic IDs, enabling
    /// upsert-by-ID semantics for label membership updates.
    pub fn upload_batch(&self, chunks: &[(crate::engine::Chunk, Vec<f32>)]) -> Result<u64> {
        if chunks.is_empty() {
            return Ok(0);
        }

        let points: Vec<Point> = chunks
            .iter()
            .map(|(chunk, embedding)| {
                Point {
                    id: chunk.point_id(), // Deterministic ID based on file_id + chunk_ordinal
                    vector: embedding.clone(),
                    payload: PointPayload {
                        text: chunk.text.clone(),
                        source_type: chunk.source_type.clone(),

                        // Label membership
                        catalog: chunk.catalog.clone(),
                        label_id: chunk.label_id.clone(),
                        active_label_ids: chunk.active_label_ids.clone(),

                        // Implementation identity
                        embedder_id: chunk.embedder_id.clone(),
                        chunker_id: chunk.chunker_id.clone(),

                        // Provenance
                        blob_id: chunk.blob_id.clone(),
                        content_hash: chunk.content_hash.clone(),

                        // File identity
                        file_id: chunk.file_id.clone(),

                        // Path context
                        relative_path: chunk.relative_path.clone(),
                        package_name: chunk.package_name.clone(),
                        source_uri: chunk.source_uri.clone(),

                        // Chunk metadata
                        chunk_ordinal: chunk.chunk_ordinal,
                        chunk_count: chunk.chunk_count,
                        start_line: chunk.start_line,
                        end_line: chunk.end_line,

                        // Semantic context
                        symbol_name: chunk.symbol_name.clone(),
                        chunk_type: chunk.chunk_type.clone(),
                        chunk_kind: chunk.chunk_kind.clone(),
                        breadcrumb: Some(chunk.breadcrumb.clone()),

                        // Sentinel
                        file_complete: false,
                    },
                }
            })
            .collect();

        let point_count = points.len();
        let request_body = UpsertRequest { points };

        // Use wait=true to ensure points are fully indexed before subsequent reads
        // This prevents race conditions with mark_file_complete() and add_label_to_file_chunks()
        let endpoint = format!(
            "{}/collections/{}/points?wait=true",
            self.url, self.collection
        );

        if self.debug {
            let json_body = serde_json::to_string_pretty(&request_body)
                .unwrap_or_else(|_| "<unable to serialize>".to_string());
            eprintln!("[DEBUG] Request endpoint: {}", endpoint);
            eprintln!(
                "[DEBUG] Request body (first 5000 chars): {}",
                json_body.chars().take(5000).collect::<String>()
            );
            eprintln!(
                "[DEBUG] Total request points: {}",
                request_body.points.len()
            );
        }

        let response = self.client.put(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<unable to read body>".to_string());
            if self.debug {
                eprintln!("[DEBUG] Response status: {}", status);
                eprintln!("[DEBUG] Response body: {}", body);
            }

            // Check for payload limit error - this is a fatal error requiring batch subdivision
            if is_payload_limit_error(&body) {
                return Err(anyhow!(
                    "Qdrant payload limit exceeded: {}. Batch size: {} chunks.",
                    body,
                    point_count
                ));
            }

            return Err(anyhow!(
                "Qdrant upsert failed with HTTP status {}: {}",
                status,
                body
            ));
        }

        // Parse response with error observability for malformed responses
        let response_text = response.text()?;

        if self.debug {
            eprintln!(
                "[DEBUG] Response body (first 2000 chars): {}",
                response_text.chars().take(2000).collect::<String>()
            );
        }

        let upsert_response: UpsertResponse = match serde_json::from_str(&response_text) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Failed to deserialize Qdrant upsert response: {}", e);
                eprintln!(
                    "Raw response (first 2000 chars): {}",
                    &response_text.chars().take(2000).collect::<String>()
                );
                return Err(anyhow!("Upsert response deserialization error: {}", e));
            }
        };

        if upsert_response.status != "ok" {
            return Err(anyhow!(
                "Qdrant upsert failed with status: {}",
                upsert_response.status
            ));
        }

        Ok(upsert_response.result.operation_id)
    }

    /// Mark a file as complete by setting file_complete=true on chunk #1
    ///
    /// This is called after all chunks for a file have been uploaded.
    /// Uses Qdrant's payload update API to set the field without rewriting the point.
    ///
    /// Optimized: Computes point ID directly instead of using scroll query.
    /// This eliminates the read-after-write race condition and is more efficient.
    pub fn mark_file_complete(&self, file_id: &str) -> Result<()> {
        // Compute point ID directly from file_id and chunk_ordinal=1
        // This is deterministic and matches how upload_batch creates point IDs
        let point_id = crate::engine::util::compute_point_id(file_id, 1);

        #[derive(Debug, Serialize)]
        struct SetPayloadRequest {
            payload: std::collections::HashMap<String, serde_json::Value>,
            points: Vec<String>,
        }

        let mut payload = std::collections::HashMap::new();
        payload.insert("file_complete".to_string(), serde_json::json!(true));

        let request_body = SetPayloadRequest {
            payload,
            points: vec![point_id],
        };

        let endpoint = format!(
            "{}/collections/{}/points/payload?wait=true",
            self.url, self.collection
        );

        let response = self.client.post(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to set file_complete for {}: HTTP {}",
                file_id,
                response.status()
            ));
        }

        Ok(())
    }

    /// Queries the collection with an embedding
    /// Legacy - catalog-only search, superseded by search_with_label()
    #[allow(dead_code)]
    pub fn query(
        &self,
        embedding: &[f32],
        limit: usize,
        catalog: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        #[derive(Debug, Serialize)]
        struct SearchRequest {
            vector: Vec<f32>,
            limit: usize,
            with_payload: bool,
            filter: Option<Filter>,
        }

        let filter = catalog.map(|cat| Filter {
            must: vec![Condition {
                key: "catalog".to_string(),
                r#match: MatchValue {
                    value: cat.to_string(),
                },
            }],
        });

        let request_body = SearchRequest {
            vector: embedding.to_vec(),
            limit,
            with_payload: true,
            filter,
        };

        let endpoint = format!("{}/collections/{}/points/search", self.url, self.collection);

        let response = self
            .client
            .post(&endpoint)
            .json(&request_body)
            .send()?
            .json::<SearchResponse>()?;

        Ok(response.result)
    }

    /// Get a single point by ID (legacy - uses old hash-based IDs)
    #[allow(dead_code)]
    pub fn get_point(&self, id: u64) -> Result<Option<PointResult>> {
        #[derive(Debug, Serialize)]
        struct PointRequest {
            ids: Vec<u64>,
            with_payload: bool,
        }

        let request_body = PointRequest {
            ids: vec![id],
            with_payload: true,
        };

        let endpoint = format!("{}/collections/{}/points", self.url, self.collection);

        let response = self.client.post(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Ok(None);
        }

        #[derive(Debug, Deserialize)]
        struct PointResponse {
            result: Vec<PointResult>,
        }

        let point_response: PointResponse = response.json()?;
        Ok(point_response.result.into_iter().next())
    }

    /// Get chunks by file_id with optional selector (Phase 7+)
    ///
    /// # Arguments
    /// * `file_id` - 16-char hex file ID
    /// * `selector` - Which chunks to retrieve
    ///
    /// # Returns
    /// Vector of points sorted by chunk_ordinal, or error
    /// Legacy - unfiltered by label, superseded by get_chunks_by_file_id_with_label()
    #[allow(dead_code)]
    pub fn get_chunks_by_file_id(&self, file_id: &str) -> Result<Vec<PointResult>> {
        // Build scroll request with filter on file_id
        #[derive(Debug, Serialize)]
        struct ScrollRequestWithRange {
            filter: FilterWithRange,
            with_payload: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            offset: Option<QdrantId>,
        }

        #[derive(Debug, Serialize)]
        struct FilterWithRange {
            must: Vec<serde_json::Value>,
        }

        // Build file_id condition
        let must_values = vec![serde_json::json!({
            "key": "file_id",
            "match": { "value": file_id }
        })];

        let mut results: Vec<PointResult> = Vec::new();
        let mut offset: Option<QdrantId> = None;
        const LIMIT: u32 = 100;

        loop {
            let endpoint = format!(
                "{}/collections/{}/points/scroll?limit={}",
                self.url, self.collection, LIMIT
            );

            let request_body = ScrollRequestWithRange {
                filter: FilterWithRange {
                    must: must_values.clone(),
                },
                with_payload: true,
                offset: offset.clone(),
            };

            let response = self.client.post(&endpoint).json(&request_body).send()?;

            if !response.status().is_success() {
                return Err(anyhow!(
                    "Failed to scroll file chunks: HTTP {}",
                    response.status()
                ));
            }

            let response_text = response.text()?;
            let scroll_response: ScrollResponse = match serde_json::from_str(&response_text) {
                Ok(r) => r,
                Err(e) => {
                    return Err(anyhow!("Deserialization error: {}", e));
                }
            };

            if scroll_response.result.points.is_empty() {
                break;
            }

            for point in scroll_response.result.points {
                results.push(PointResult {
                    id: point.id,
                    payload: point.payload,
                });
            }

            offset = scroll_response.result.next_page_offset;
            if offset.is_none() {
                break;
            }
        }

        // Sort by chunk_ordinal
        results.sort_by_key(|p| p.payload.chunk_ordinal);

        Ok(results)
    }

    // ========================================
    // Label-aware operations (Phase 2)
    // ========================================

    /// Upsert label metadata point
    ///
    /// Creates or updates a metadata point for a label. The label_id is used
    /// directly as the point ID for easy lookup.
    pub fn upsert_label_metadata(&self, metadata: &LabelMetadata) -> Result<()> {
        let endpoint = format!("{}/collections/{}/points", self.url, self.collection);

        // Create a zero vector (768 dimensions) - required by Qdrant but never used in search
        let zero_vector: Vec<f32> = vec![0.0; 768];

        #[derive(Debug, Serialize)]
        struct LabelPoint {
            id: String,
            vector: Vec<f32>,
            payload: LabelMetadata,
        }

        // Convert label_id to UUID for Qdrant compatibility
        let point_id = super::util::string_to_uuid(&metadata.label_id);

        let point = LabelPoint {
            id: point_id,
            vector: zero_vector,
            payload: metadata.clone(),
        };

        #[derive(Debug, Serialize)]
        struct UpsertLabelRequest {
            points: Vec<LabelPoint>,
        }

        let request_body = UpsertLabelRequest {
            points: vec![point],
        };

        let response = self.client.put(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to upsert label metadata: HTTP {}",
                response.status()
            ));
        }

        Ok(())
    }

    /// Get label metadata by label_id
    #[allow(dead_code)]
    pub fn get_label_metadata(&self, label_id: &str) -> Result<Option<LabelMetadata>> {
        // Convert label_id to UUID the same way upsert_label_metadata does
        let point_id = super::util::string_to_uuid(label_id);
        let endpoint = format!(
            "{}/collections/{}/points/{}",
            self.url, self.collection, point_id
        );

        let response = self.client.get(&endpoint).send()?;

        if !response.status().is_success() {
            // Point doesn't exist
            return Ok(None);
        }

        #[derive(Debug, Deserialize)]
        struct LabelPointResponse {
            result: LabelPointResult,
        }

        #[derive(Debug, Deserialize)]
        struct LabelPointResult {
            payload: LabelMetadata,
        }

        let label_response: LabelPointResponse = response.json()?;
        Ok(Some(label_response.result.payload))
    }

    /// Get sentinel (chunk 1) for a file to check if already indexed
    ///
    /// Returns FileSyncInfo if the sentinel exists and is complete.
    /// Optimized: Computes point ID directly instead of using scroll query.
    pub fn get_file_sentinel(&self, file_id: &str) -> Result<Option<FileSyncInfo>> {
        // Compute point ID directly from file_id and chunk_ordinal=1
        let point_id = crate::engine::util::compute_point_id(file_id, 1);

        // Use Qdrant's point retrieval API with string ID
        let endpoint = format!(
            "{}/collections/{}/points/{}",
            self.url, self.collection, point_id
        );

        let response = self.client.get(&endpoint).send()?;

        if !response.status().is_success() {
            // Point doesn't exist
            return Ok(None);
        }

        #[derive(Debug, Deserialize)]
        struct PointResponse {
            result: PointResult,
        }

        let point_response: PointResponse = response.json()?;

        if point_response.result.payload.file_complete {
            return Ok(Some(FileSyncInfo {
                content_hash: point_response.result.payload.content_hash.clone(),
                file_complete: true,
            }));
        }

        Ok(None)
    }

    /// Add a label to a chunk's active_label_ids
    /// Add a label to all chunks for a file (by file_id)
    pub fn add_label_to_file_chunks(&self, file_id: &str, label_id: &str) -> Result<()> {
        // Get all chunks for this file with their current labels
        // We need to read-modify-write to properly append the label
        let mut chunks_to_update: Vec<(String, Vec<String>)> = Vec::new();
        let mut offset: Option<QdrantId> = None;
        const LIMIT: u32 = 100;

        loop {
            #[derive(Debug, Serialize)]
            struct ScrollForLabels {
                filter: FilterForLabels,
                with_payload: bool,
                #[serde(skip_serializing_if = "Option::is_none")]
                offset: Option<QdrantId>,
            }

            #[derive(Debug, Serialize)]
            struct FilterForLabels {
                must: Vec<serde_json::Value>,
            }

            let must_values = vec![
                serde_json::json!({
                    "key": "file_id",
                    "match": { "value": file_id }
                }),
                serde_json::json!({
                    "key": "source_type",
                    "match": { "value": "code" }
                }),
            ];

            let endpoint = format!(
                "{}/collections/{}/points/scroll?limit={}",
                self.url, self.collection, LIMIT
            );

            let request_body = ScrollForLabels {
                filter: FilterForLabels { must: must_values },
                with_payload: true, // Need payload to get current labels
                offset: offset.clone(),
            };

            let response = self.client.post(&endpoint).json(&request_body).send()?;

            if !response.status().is_success() {
                return Err(anyhow!(
                    "Failed to scroll file chunks: HTTP {}",
                    response.status()
                ));
            }

            #[derive(Debug, Deserialize)]
            struct LabelsResponse {
                result: LabelsResult,
            }

            #[derive(Debug, Deserialize)]
            struct LabelsResult {
                points: Vec<LabelPoint>,
                #[serde(default)]
                next_page_offset: Option<QdrantId>,
            }

            #[derive(Debug, Deserialize)]
            struct LabelPoint {
                id: QdrantId,
                payload: LabelPayload,
            }

            #[derive(Debug, Deserialize)]
            struct LabelPayload {
                #[serde(default, deserialize_with = "deserialize_label_ids")]
                active_label_ids: Vec<String>,
            }

            let labels_response: LabelsResponse = response.json()?;

            if labels_response.result.points.is_empty() {
                break;
            }

            for point in labels_response.result.points {
                let id_str = match point.id {
                    QdrantId::String(s) => s,
                    QdrantId::Integer(n) => n.to_string(),
                };
                chunks_to_update.push((id_str, point.payload.active_label_ids));
            }

            offset = labels_response.result.next_page_offset;
            if offset.is_none() {
                break;
            }
        }

        // Now update each chunk, appending the new label if not already present
        for (point_id, mut current_labels) in chunks_to_update {
            if !current_labels.contains(&label_id.to_string()) {
                current_labels.push(label_id.to_string());
            }
            self.set_active_labels(&point_id, &current_labels)?;
        }

        Ok(())
    }

    /// Remove a label from chunks where it's in active_label_ids
    ///
    /// This scans all chunks with the label and removes the label from active_label_ids.
    /// If active_label_ids becomes empty, the chunk is deleted.
    ///
    /// # Arguments
    /// * `label_id` - The label to remove
    /// * `exclude_file_ids` - File IDs to skip (files that were touched in the current crawl)
    ///
    /// # Returns
    /// Number of chunks processed
    pub fn remove_label_from_chunks(
        &self,
        label_id: &str,
        exclude_file_ids: &std::collections::HashSet<String>,
    ) -> Result<u64> {
        let mut processed: u64 = 0;
        let mut offset: Option<QdrantId> = None;
        const LIMIT: u32 = 100;

        loop {
            // Scroll for chunks with this label
            #[derive(Debug, Serialize)]
            struct ScrollWithLabel {
                filter: LabelFilter,
                with_payload: bool,
                #[serde(skip_serializing_if = "Option::is_none")]
                offset: Option<QdrantId>,
            }

            #[derive(Debug, Serialize)]
            struct LabelFilter {
                must: Vec<serde_json::Value>,
            }

            let must_values = vec![
                serde_json::json!({
                    "key": "active_label_ids",
                    "match": { "value": label_id }
                }),
                serde_json::json!({
                    "key": "source_type",
                    "match": { "value": "code" }
                }),
            ];

            let endpoint = format!(
                "{}/collections/{}/points/scroll?limit={}",
                self.url, self.collection, LIMIT
            );

            let request_body = ScrollWithLabel {
                filter: LabelFilter { must: must_values },
                with_payload: true,
                offset: offset.clone(),
            };

            let response = self.client.post(&endpoint).json(&request_body).send()?;

            if !response.status().is_success() {
                return Err(anyhow!(
                    "Failed to scroll chunks with label: HTTP {}",
                    response.status()
                ));
            }

            let response_text = response.text()?;
            let scroll_response: ScrollResponse = match serde_json::from_str(&response_text) {
                Ok(r) => r,
                Err(e) => {
                    return Err(anyhow!("Deserialization error: {}", e));
                }
            };

            if scroll_response.result.points.is_empty() {
                break;
            }

            for point in scroll_response.result.points {
                let file_id = point.payload.file_id.clone();

                // Skip if this file was touched in the current crawl
                if exclude_file_ids.contains(&file_id) {
                    continue;
                }

                // Remove label from active_label_ids
                let mut new_labels = point.payload.active_label_ids.clone();
                new_labels.retain(|l| l != label_id);

                let point_id_str = match point.id {
                    QdrantId::String(s) => s,
                    QdrantId::Integer(n) => n.to_string(),
                };

                if new_labels.is_empty() {
                    // Delete the chunk
                    self.delete_point(&point_id_str)?;
                } else {
                    // Update active_label_ids
                    self.set_active_labels(&point_id_str, &new_labels)?;
                }

                processed += 1;
            }

            offset = scroll_response.result.next_page_offset;
            if offset.is_none() {
                break;
            }
        }

        Ok(processed)
    }

    /// Delete a single point by ID
    fn delete_point(&self, point_id: &str) -> Result<()> {
        let endpoint = format!("{}/collections/{}/points/delete", self.url, self.collection);

        #[derive(Debug, Serialize)]
        struct DeletePointRequest {
            points: Vec<String>,
        }

        let request_body = DeletePointRequest {
            points: vec![point_id.to_string()],
        };

        let response = self.client.post(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to delete point: HTTP {}",
                response.status()
            ));
        }

        Ok(())
    }

    /// Set active_label_ids on a point
    fn set_active_labels(&self, point_id: &str, labels: &[String]) -> Result<()> {
        // Use wait=true to ensure the update is visible before subsequent operations
        let endpoint = format!(
            "{}/collections/{}/points/payload?wait=true",
            self.url, self.collection
        );

        #[derive(Debug, Serialize)]
        struct SetLabelsRequest {
            payload: serde_json::Value,
            points: Vec<String>,
        }

        let payload = serde_json::json!({
            "active_label_ids": labels
        });

        let request_body = SetLabelsRequest {
            payload,
            points: vec![point_id.to_string()],
        };

        let response = self.client.post(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to set active labels: HTTP {}",
                response.status()
            ));
        }

        Ok(())
    }

    /// Search with label filtering
    ///
    /// Filters by catalog, label (via active_label_ids), and source_type.
    pub fn search_with_label(
        &self,
        embedding: &[f32],
        limit: usize,
        catalog: &str,
        label_id: &str,
    ) -> Result<Vec<SearchResult>> {
        #[derive(Debug, Serialize)]
        struct LabelSearchRequest {
            vector: Vec<f32>,
            limit: usize,
            with_payload: bool,
            filter: LabelSearchFilter,
        }

        #[derive(Debug, Serialize)]
        struct LabelSearchFilter {
            must: Vec<serde_json::Value>,
        }

        let must_values = vec![
            serde_json::json!({
                "key": "catalog",
                "match": { "value": catalog }
            }),
            serde_json::json!({
                "key": "active_label_ids",
                "match": { "value": label_id }
            }),
            serde_json::json!({
                "key": "source_type",
                "match": { "value": "code" }
            }),
        ];

        let request_body = LabelSearchRequest {
            vector: embedding.to_vec(),
            limit,
            with_payload: true,
            filter: LabelSearchFilter { must: must_values },
        };

        let endpoint = format!("{}/collections/{}/points/search", self.url, self.collection);

        let response = self.client.post(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to search with label: HTTP {}",
                response.status()
            ));
        }

        let search_response: SearchResponse = response.json()?;
        Ok(search_response.result)
    }

    /// Get chunks by file_id filtered by active_label_ids
    ///
    /// Returns chunks that belong to the specified label, sorted by chunk_ordinal.
    pub fn get_chunks_by_file_id_with_label(
        &self,
        file_id: &str,
        label_id: &str,
    ) -> Result<Vec<PointResult>> {
        #[derive(Debug, Serialize)]
        struct ScrollWithLabelFilter {
            filter: LabelFilterCondition,
            with_payload: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            offset: Option<QdrantId>,
        }

        #[derive(Debug, Serialize)]
        struct LabelFilterCondition {
            must: Vec<serde_json::Value>,
        }

        let must_values = vec![
            serde_json::json!({
                "key": "file_id",
                "match": { "value": file_id }
            }),
            serde_json::json!({
                "key": "active_label_ids",
                "match": { "value": label_id }
            }),
            serde_json::json!({
                "key": "source_type",
                "match": { "value": "code" }
            }),
        ];

        let mut results: Vec<PointResult> = Vec::new();
        let mut offset: Option<QdrantId> = None;
        const LIMIT: u32 = 100;

        loop {
            let endpoint = format!(
                "{}/collections/{}/points/scroll?limit={}",
                self.url, self.collection, LIMIT
            );

            let request_body = ScrollWithLabelFilter {
                filter: LabelFilterCondition {
                    must: must_values.clone(),
                },
                with_payload: true,
                offset: offset.clone(),
            };

            let response = self.client.post(&endpoint).json(&request_body).send()?;

            if !response.status().is_success() {
                return Err(anyhow!(
                    "Failed to scroll file chunks: HTTP {}",
                    response.status()
                ));
            }

            let response_text = response.text()?;
            let scroll_response: ScrollResponse = match serde_json::from_str(&response_text) {
                Ok(r) => r,
                Err(e) => {
                    return Err(anyhow!("Deserialization error: {}", e));
                }
            };

            if scroll_response.result.points.is_empty() {
                break;
            }

            for point in scroll_response.result.points {
                results.push(PointResult {
                    id: point.id,
                    payload: point.payload,
                });
            }

            offset = scroll_response.result.next_page_offset;
            if offset.is_none() {
                break;
            }
        }

        // Sort by chunk_ordinal
        results.sort_by_key(|p| p.payload.chunk_ordinal);

        Ok(results)
    }
}

/// A point retrieved by ID (no score, unlike SearchResult)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PointResult {
    pub id: QdrantId,
    pub payload: PointPayload,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_label_metadata_id_round_trip_uses_same_uuid_strategy() {
        let label_id = "rushstack:feature/foo";
        let metadata = LabelMetadata {
            source_type: "label-metadata".to_string(),
            catalog: "rushstack".to_string(),
            label_id: label_id.to_string(),
            label_name: "feature/foo".to_string(),
            commit_oid: "abc123".to_string(),
            source_kind: "git-commit".to_string(),
            crawl_complete: false,
            updated_at_unix_secs: 123,
        };

        let upsert_point_id = crate::engine::util::string_to_uuid(&metadata.label_id);
        let get_point_id = crate::engine::util::string_to_uuid(label_id);

        assert_eq!(upsert_point_id, get_point_id);
        assert_eq!(upsert_point_id.len(), 36);
        assert!(upsert_point_id.contains('-'));
    }

    #[test]
    fn test_is_payload_limit_error_detects_qdrant_error() {
        // Real Qdrant error response
        let body = r#"{"status":{"error":"Payload error: JSON payload (36704120 bytes) is larger than allowed (limit: 33554432 bytes)."},"time":0.0}"#;
        assert!(is_payload_limit_error(body));

        // Also works with plain text format
        let text = "Payload error: JSON payload (36704120 bytes) is larger than allowed (limit: 33554432 bytes).";
        assert!(is_payload_limit_error(text));
    }

    #[test]
    fn test_is_payload_limit_error_rejects_other_errors() {
        // Connection error
        let body = r#"{"status":{"error":"Connection refused"}}"#;
        assert!(!is_payload_limit_error(body));

        // Different payload error
        let body = r#"{"status":{"error":"Invalid payload format"}}"#;
        assert!(!is_payload_limit_error(body));

        // Unrelated error
        let body = r#"{"status":{"error":"Collection not found"}}"#;
        assert!(!is_payload_limit_error(body));
    }
}
