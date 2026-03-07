//! Qdrant client for batch uploading embeddings
//! 
//! This module handles HTTP communication with Qdrant to upload
//! chunks with their embeddings for semantic search.

use anyhow::{anyhow, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use super::util::compute_chunk_id;

const DEFAULT_QDRANT_URL: &str = "http://localhost:6333";

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

/// Request body for scroll operations
#[derive(Debug, Serialize)]
struct ScrollRequest {
    filter: Filter,
    with_payload: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<QdrantId>,
}

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
    id: u64,  // Hash-based ID
    vector: Vec<f32>,
    payload: PointPayload,
}

/// Payload associated with a point
#[derive(Debug, Serialize, Deserialize)]
pub struct PointPayload {
    pub text: String,
    pub source_uri: String,
    pub source_type: String,
    pub catalog: String,
    pub content_hash: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    pub chunk_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breadcrumb: Option<String>,
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

impl QdrantId {
    /// Returns the integer value if this is an Integer variant
    fn as_u64(&self) -> Option<u64> {
        match self {
            QdrantId::Integer(n) => Some(*n),
            QdrantId::String(_) => None,
        }
    }
}

impl std::ops::Shr<i32> for QdrantId {
    type Output = u64;
    
    fn shr(self, rhs: i32) -> Self::Output {
        match self {
            QdrantId::Integer(n) => n >> rhs,
            QdrantId::String(_) => 0,
        }
    }
}

impl std::ops::Shr<i32> for &QdrantId {
    type Output = u64;
    
    fn shr(self, rhs: i32) -> Self::Output {
        match self {
            QdrantId::Integer(n) => *n >> rhs,
            QdrantId::String(_) => 0,
        }
    }
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
    #[allow(dead_code)]
    pub fn delete_catalog(&self, catalog: &str) -> Result<u64> {
        let endpoint = format!("{}/collections/{}/points/delete", self.url, self.collection);

        let request_body = FilterRequest {
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
    pub fn delete_file(&self, file_path: &str, catalog: &str) -> Result<u64> {
        let endpoint = format!("{}/collections/{}/points/delete", self.url, self.collection);

        let request_body = FilterRequest {
            filter: Filter {
                must: vec![
                    Condition {
                        key: "catalog".to_string(),
                        r#match: MatchValue { value: catalog.to_string() },
                    },
                    Condition {
                        key: "source_uri".to_string(),
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
    /// Returns a map of file path → content hash
    pub fn get_catalog_files(&self, catalog: &str) -> Result<std::collections::HashMap<String, String>> {
        let mut files = std::collections::HashMap::new();
        let mut offset: Option<QdrantId> = None;
        const LIMIT: u32 = 1000;

        loop {
            let endpoint = format!(
                "{}/collections/{}/points/scroll?limit={}",
                self.url, self.collection, LIMIT
            );

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

            let response_text = response.text()?;
            let scroll_response: ScrollResponse = match serde_json::from_str(&response_text) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Failed to deserialize Qdrant response: {}", e);
                    eprintln!("Raw response (first 2000 chars): {}", &response_text.chars().take(2000).collect::<String>());
                    return Err(anyhow!("Deserialization error: {}", e));
                }
            };

            if scroll_response.result.points.is_empty() {
                break;
            }

            for point in scroll_response.result.points {
                files.insert(point.payload.source_uri.clone(), point.payload.content_hash.clone());
            }

            offset = scroll_response.result.next_page_offset;
            if offset.is_none() {
                break;
            }
        }

        Ok(files)
    }

    /// Uploads a batch of chunks with their embeddings
    pub fn upload_batch(&self, chunks: &[(crate::engine::Chunk, Vec<f32>)]) -> Result<u64> {
        if chunks.is_empty() {
            return Ok(0);
        }

        let points: Vec<Point> = chunks
            .iter()
            .map(|(chunk, embedding)| {
                let id = compute_chunk_id(&chunk.source_uri, chunk.start_line, chunk.part_number);
                Point {
                    id,
                    vector: embedding.clone(),
                    payload: PointPayload {
                        text: chunk.text.clone(),
                        source_uri: chunk.source_uri.clone(),
                        source_type: chunk.source_type.clone(),
                        catalog: chunk.catalog.clone(),
                        content_hash: chunk.content_hash.clone(),
                        start_line: chunk.start_line,
                        end_line: chunk.end_line,
                        symbol_name: chunk.symbol_name.clone(),
                        chunk_type: chunk.chunk_type.clone(),
                        breadcrumb: Some(chunk.breadcrumb.clone()),
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

    /// Get a single point by ID
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
        
        let response = self
            .client
            .post(&endpoint)
            .json(&request_body)
            .send()?;
        
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
}

/// A point retrieved by ID (no score, unlike SearchResult)
#[derive(Debug, Deserialize)]
pub struct PointResult {
    pub id: QdrantId,
    pub payload: PointPayload,
}
