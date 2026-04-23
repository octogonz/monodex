//! Search operations for Qdrant points.
//!
//! Edit here when: Changing search queries, result formatting, or point retrieval.
//! Do not edit here for: Upload logic (upload.rs), file operations (file_ops.rs), label operations (label_ops.rs).

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::client::QdrantUploader;
use super::models::{PointResult, QdrantId, SearchResult};

impl QdrantUploader {
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
            filter: Option<super::models::Filter>,
        }

        let filter = catalog.map(|cat| super::models::Filter {
            must: vec![super::models::Condition {
                key: "catalog".to_string(),
                r#match: super::models::MatchValue {
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
            .json::<super::models::SearchResponse>()?;

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

        let search_response: super::models::SearchResponse = response.json()?;

        // Validate each payload
        for result in &search_response.result {
            result.payload.validate()?;
        }

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
            let scroll_response: super::models::ScrollResponse =
                match serde_json::from_str(&response_text) {
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
