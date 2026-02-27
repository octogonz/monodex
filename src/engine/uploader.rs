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

/// Generates a deterministic UUID from a string
fn string_to_uuid(s: &str) -> uuid::Uuid {
    let mut hasher = XxHash64::default();
    s.hash(&mut hasher);
    let hash = hasher.finish();
    
    // Convert to UUID v4 format (using hash as bytes)
    let bytes = hash.to_be_bytes();
    let mut uuid_bytes = [0u8; 16];
    
    // Copy hash bytes into UUID (truncate/extend as needed)
    let hash_slice: [u8; 8] = hash.to_be_bytes();
    uuid_bytes[0..8].copy_from_slice(&hash_slice);
    uuid_bytes[8..16].copy_from_slice(&hash_slice);
    
    uuid::Uuid::from_bytes(uuid_bytes)
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
    id: uuid::Uuid,
    vector: Vec<f32>,
    payload: PointPayload,
}

/// Payload associated with a point
#[derive(Debug, Serialize, Deserialize)]
pub struct PointPayload {
    pub text: String,
    pub file: String,
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

    /// Purges all points from the collection
    ///
    /// Deletes all existing points from the collection. This is useful
    /// for full re-indexing.
    pub fn purge(&self) -> Result<()> {
        let endpoint = format!("{}/collections/{}/points/delete", self.url, self.collection);

        // Delete with empty filter = delete all points
        #[derive(Debug, Serialize)]
        struct DeleteRequest {
            filter: serde_json::Value,
        }

        let request_body = DeleteRequest {
            filter: serde_json::json!({}),  // Empty filter matches all
        };

        let response = self.client.post(&endpoint).json(&request_body).send()?;

        if !response.status().is_success() {
            return Err(anyhow!("Failed to purge collection: HTTP {}", response.status()));
        }

        Ok(())
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
                let id_string = format!("{}:{}-{}", chunk.file, chunk.start_line, chunk.end_line);
                Point {
                    id: string_to_uuid(&id_string),
                    vector: embedding.clone(),
                    payload: PointPayload {
                        text: chunk.text.clone(),
                        file: chunk.file.clone(),
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
    ///
    /// # Returns
    ///
    /// Vector of search results with scores and payloads
    pub fn query(&self, embedding: &[f32], limit: usize) -> Result<Vec<SearchResult>> {
        #[derive(Debug, Serialize)]
        struct SearchRequest {
            vector: Vec<f32>,
            limit: usize,
            with_payload: bool,
        }

        let request_body = SearchRequest {
            vector: embedding.to_vec(),
            limit,
            with_payload: true,
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
