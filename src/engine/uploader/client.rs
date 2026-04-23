//! HTTP client for Qdrant API operations.
//!
//! Edit here when: Changing client configuration, timeout settings, or low-level HTTP handling.
//! Do not edit here for: Upload logic (upload.rs), file operations (file_ops.rs), label operations (label_ops.rs).

//! HTTP client for Qdrant API operations.
//!
//! Edit here when: Changing client configuration, timeout settings, or low-level HTTP handling.
//! Do not edit here for: Upload logic (upload.rs), file operations (file_ops.rs), label operations (label_ops.rs), search (search.rs).

use anyhow::{anyhow, Result};
use reqwest::blocking::Client;

use super::models::{is_payload_limit_error, UpsertResponse};

pub const DEFAULT_QDRANT_URL: &str = "http://localhost:6333";

/// Qdrant client for uploading embeddings
pub struct QdrantUploader {
    pub(super) client: Client,
    pub(super) url: String,
    pub(super) collection: String,
    pub(super) debug: bool,
    pub(super) max_upload_bytes: usize,
}

impl QdrantUploader {
    /// Creates a new Qdrant uploader
    pub fn new(
        collection: &str,
        qdrant_url: Option<&str>,
        debug: bool,
        max_upload_bytes: usize,
    ) -> Result<Self> {
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
            max_upload_bytes,
        })
    }

    /// Get the max upload bytes limit
    pub fn max_upload_bytes(&self) -> usize {
        self.max_upload_bytes
    }

    /// Send pre-serialized upload batch to Qdrant (helper for upload_batch)
    pub(super) fn send_upload_batch(&self, bytes: &[u8]) -> Result<u64> {
        let endpoint = format!(
            "{}/collections/{}/points?wait=true",
            self.url, self.collection
        );

        if self.debug {
            eprintln!("[DEBUG] Request endpoint: {}", endpoint);
            eprintln!("[DEBUG] Request body size: {} bytes", bytes.len());
        }

        let response = self
            .client
            .put(&endpoint)
            .body(bytes.to_vec())
            .header("Content-Type", "application/json")
            .send()?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<unable to read body>".to_string());
            if self.debug {
                eprintln!("[DEBUG] Response status: {}", status);
                eprintln!("[DEBUG] Response body: {}", body);
            }

            // Check for payload limit error
            if is_payload_limit_error(&body) {
                return Err(anyhow!("Qdrant payload limit exceeded: {}", body));
            }

            return Err(anyhow!(
                "Qdrant upsert failed with HTTP status {}: {}",
                status,
                body
            ));
        }

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
}
