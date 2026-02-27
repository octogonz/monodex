//! Qdrant client for batch uploading embeddings
//!
//! This module handles HTTP communication with Qdrant to upload
//! chunks with their embeddings for semantic search.

use anyhow::{anyhow, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use twox_hash::XxHash64;
use std::hash::{Hash, Hasher};

const DEFAULT_QDRANT_URL: &str = "http://localhost:6333";

/// Qdrant client for uploading embeddings
pub struct QdrantUploader {
    client: Client,
    url: String,
    collection: String,
}

/// Request body for Qdrant upsert operation
#[derive(Debug, Serialize)]
struct UpsertRequest {
    points: Vec<Point>,
}

/// A single point in Qdrant
#[derive(Debug, Serialize)]
struct Point {
    id: uuid::Uuid,
    vector: Vec<f32>,
    payload: PointPayload,
}

/// Payload associated with a point
#[derive(Debug, Serialize, Deserialize)]
pub struct PointPayload {
    pub text: String,
    pub file: String,
    pub catalog: String,
    pub content_hash: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    pub chunk_type: String,
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

/// Request for filtering points
#[derive(Debug, Serialize)]
struct FilterRequest {
    filter: Filter,
}

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

/// Response from scroll (list points)
#[derive(Debug, Deserialize)]
struct ScrollResponse {
    result: ScrollResult,
    status: String,
}

#[derive(Debug, Deserialize)]
struct ScrollResult {
    points: Vec<ScrollPoint>,
    #[serde(default)]
    next_page_offset: Option<String>,
}

/// Scroll point from Qdrant
#[derive(Debug, Deserialize)]
struct ScrollPoint {
    id: String,
    payload: PointPayload,
}

/// Response from delete
#[derive(Debug, Deserialize)]
struct DeleteResponse {
    result: DeleteResult,
    status: String,
}

#[derive(Debug, Deserialize)]
struct DeleteResult {
    operation_id: u64,
}

/// Response from Qdrant search
#[derive(Debug, Deserialize)]
struct SearchResponse {
    result: Vec<SearchResult>,
    status: String,
}

/// Search result from Qdrant
#[derive(Debug, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub payload: PointPayload,
}

impl QdrantUploader {
    /// Creates a new Qdrant uploader
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the Qdrant collection
    /// * `qdrant_url` - Optional Qdrant URL (defaults to localhost:6333)
    pub fn new(collection: &str, qdrant_url: Option<&str>) -> Result<Self> {
        let url = qdrant_url.unwrap_or(DEFAULT_QDRANT_URL).to_string();
        
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        Ok(Self {
            client,
            url,
            collection: collection.to_string(),
        })
    }

    /// Delete all points for a specific catalog
    ///
    /// # Arguments
    ///
    /// * `catalog` - Catalog name to delete
    pub fn delete_catalog(&self, catalog: &str) -> Result<u64> {
        let endpoint = format!("{}/collections/{}/points/delete", self.url, self.collection);

        #[derive(Debug, Serialize)]
        struct DeleteRequest {
            filter: Filter,
        }

        let request_body = DeleteRequest {
            filter: Filter {
                must: vec![
                    Condition {
                        key: "catalog".to_string(),
                        r#match: MatchValue { value: catalog.to_string() },
                    }
                ],
            },
        };

        let response = self.client.post(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Err(anyhow!("Failed to delete catalog: HTTP {}", response.status()));
        }

        let delete_response: DeleteResponse = response.json()?;
        Ok(delete_response.result.operation_id)
    }

    /// Delete all points for a specific file
    ///
    /// # Arguments
    ///
    /// * `file_path` - File path to delete
    /// * `catalog` - Catalog containing the file
    pub fn delete_file(&self, file_path: &str, catalog: &str) -> Result<u64> {
        let endpoint = format!("{}/collections/{}/points/delete", self.url, self.collection);

        #[derive(Debug, Serialize)]
        struct DeleteRequest {
            filter: Filter,
        }

        let request_body = DeleteRequest {
            filter: Filter {
                must: vec![
                    Condition {
                        key: "catalog".to_string(),
                        r#match: MatchValue { value: catalog.to_string() },
                    },
                    Condition {
                        key: "file".to_string(),
                        r#match: MatchValue { value: file_path.to_string() },
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
    ///
    /// Returns a map of file path → content hash
    pub fn get_catalog_files(&self, catalog: &str) -> Result<std::collections::HashMap<String, String>> {
        let mut files = std::collections::HashMap::new();
        let mut offset: Option<String> = None;
        const LIMIT: u32 = 1000;

        loop {
            let endpoint = format!(
                "{}/collections/{}/points/scroll?limit={}",
                self.url, self.collection, LIMIT
            );

            #[derive(Debug, Serialize)]
            struct ScrollRequest {
                filter: Filter,
                with_payload: bool,
                #[serde(skip_serializing_if = "Option::is_none")]
                offset: Option<String>,
            }

            let request_body = ScrollRequest {
                filter: Filter {
                    must: vec![Condition {
                        key: "catalog".to_string(),
                        r#match: MatchValue { value: catalog.to_string() },
                    }],
                },
                with_payload: true,
                offset: offset.clone(),
            };

            let response = self.client.post(&endpoint).json(&request_body).send()?;

            if !response.status().is_success() {
                return Err(anyhow!("Failed to scroll catalog: HTTP {}", response.status()));
            }

            let scroll_response: ScrollResponse = response.json()?;

            if scroll_response.result.points.is_empty() {
                break;
            }

            for point in scroll_response.result.points {
                files.insert(point.payload.file.clone(), point.payload.content_hash.clone());
            }

            offset = scroll_response.result.next_page_offset;
            if offset.is_none() {
                break;
            }
        }

        Ok(files)
    }

    /// Uploads a batch of chunks with their embeddings
    ///
    /// # Arguments
    ///
    /// * `chunks` - Vector of chunks with their associated embeddings
    ///
    /// # Returns
    ///
    /// Result containing the operation ID or an error
    pub fn upload_batch(&self, chunks: &[(crate::engine::Chunk, Vec<f32>)]) -> Result<u64> {
        if chunks.is_empty() {
            return Ok(0);
        }

        let points: Vec<Point> = chunks
            .iter()
            .map(|(chunk, embedding)| {
                Point {
                    id: uuid::Uuid::new_v4(), // Use random UUID (dedup via content_hash)
                    vector: embedding.clone(),
                    payload: PointPayload {
                        text: chunk.text.clone(),
                        file: chunk.file.clone(),
                        catalog: chunk.catalog.clone(),
                        content_hash: chunk.content_hash.clone(),
                        start_line: chunk.start_line,
                        end_line: chunk.end_line,
                        symbol_name: chunk.symbol_name.clone(),
                        chunk_type: chunk.chunk_type.clone(),
                    },
                }
            })
            .collect();

        let request_body = UpsertRequest { points };

        let endpoint = format!("{}/collections/{}/points", self.url, self.collection);
        
        let response = self
            .client
            .put(&endpoint)
            .json(&request_body)
            .send()?
            .json::<UpsertResponse>()?;

        if response.status != "ok" {
            return Err(anyhow!("Qdrant upsert failed with status: {}", response.status));
        }

        Ok(response.result.operation_id)
    }

    /// Queries the collection with an embedding
    ///
    /// # Arguments
    ///
    /// * `embedding` - Query embedding vector
    /// * `limit` - Maximum number of results
    /// * `catalog` - Optional catalog filter
    ///
    /// # Returns
    ///
    /// Vector of search results with scores and payloads
    pub fn query(&self, embedding: &[f32], limit: usize, catalog: Option<&str>) -> Result<Vec<SearchResult>> {
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
                r#match: MatchValue { value: cat.to_string() },
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
}
