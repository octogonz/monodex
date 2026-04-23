//! Label-level operations for Qdrant points.
//!
//! Edit here when: Changing label metadata, label membership, or label reassignment.
//! Do not edit here for: Upload logic (upload.rs), file operations (file_ops.rs), search (search.rs).

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use super::client::QdrantUploader;
use super::models::{LabelMetadata, QdrantId, ScrollResponse};

impl QdrantUploader {
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
        let point_id = crate::engine::util::string_to_uuid(&metadata.label_id);

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
        let point_id = crate::engine::util::string_to_uuid(label_id);
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
        let metadata = label_response.result.payload;

        // Validate identifiers from stored data
        metadata.validate()?;

        Ok(Some(metadata))
    }

    /// Add a label to all chunks for a file (by file_id)
    pub fn add_label_to_file_chunks(&self, file_id: &str, label_id: &str) -> Result<()> {
        // Get all chunks for this file with their current labels
        // We need to read-modify-write to properly append the label
        // Each entry: (point_id, active_label_ids, is_sentinel)
        let mut chunks_to_update: Vec<(String, Vec<String>, bool)> = Vec::new();
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
                #[serde(default, deserialize_with = "super::models::deserialize_label_ids")]
                active_label_ids: Vec<String>,
                #[serde(default)]
                file_complete: bool,
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
                chunks_to_update.push((
                    id_str,
                    point.payload.active_label_ids,
                    point.payload.file_complete,
                ));
            }

            offset = labels_response.result.next_page_offset;
            if offset.is_none() {
                break;
            }
        }

        // Now update each chunk, appending the new label if not already present.
        // IMPORTANT: Process sentinel (file_complete=true) LAST to ensure correct
        // incremental behavior - sentinel presence implies all chunks are labeled.

        // Partition into non-sentinel and sentinel chunks
        let mut non_sentinel: Vec<(String, Vec<String>)> = Vec::new();
        let mut sentinel: Option<(String, Vec<String>)> = None;

        for (point_id, current_labels, is_sentinel) in chunks_to_update {
            if is_sentinel {
                if sentinel.is_some() {
                    // Multiple sentinels found - this indicates data corruption or a bug
                    // The last sentinel wins, previous ones are overwritten (not skipped)
                    eprintln!(
                        "Warning: Multiple sentinel chunks found for file_id={}, \
                         this indicates data corruption. Previous sentinel will be replaced by point {}.",
                        file_id, point_id
                    );
                }
                sentinel = Some((point_id, current_labels));
            } else {
                non_sentinel.push((point_id, current_labels));
            }
        }

        // Update non-sentinel chunks first
        for (point_id, mut current_labels) in non_sentinel {
            if !current_labels.contains(&label_id.to_string()) {
                current_labels.push(label_id.to_string());
            }
            self.set_active_labels(&point_id, &current_labels)?;
        }

        // Update sentinel last
        if let Some((point_id, mut current_labels)) = sentinel {
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
}
