//! File-level operations for Qdrant points.
//!
//! Edit here when: Changing file deletion, file sync info, or sentinel operations.
//! Do not edit here for: Upload logic (upload.rs), label operations (label_ops.rs), search (search.rs).

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::client::QdrantUploader;
use super::models::{
    Condition, FileSyncInfo, Filter, FilterRequest, MatchValue, PointResult, QdrantId,
    ScrollResponse,
};

impl QdrantUploader {
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

        let delete_response: super::models::DeleteResponse = response.json()?;
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

        let delete_response: super::models::DeleteResponse = response.json()?;
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
                        active_label_ids: point.payload.active_label_ids.clone(),
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

        // Validate the payload from storage
        point_response.result.payload.validate()?;

        if point_response.result.payload.file_complete {
            return Ok(Some(FileSyncInfo {
                content_hash: point_response.result.payload.content_hash.clone(),
                file_complete: true,
                active_label_ids: point_response.result.payload.active_label_ids.clone(),
            }));
        }

        Ok(None)
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

        // Validate each payload
        for result in &results {
            result.payload.validate()?;
        }

        Ok(results)
    }
}
