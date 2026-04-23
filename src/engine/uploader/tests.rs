//! Tests for Qdrant uploader operations.

use super::client::QdrantUploader;
use super::models::{is_payload_limit_error, LabelMetadata, Point, PointPayload};
use crate::engine::Chunk;

#[test]
fn test_label_metadata_id_round_trip_uses_same_uuid_strategy() {
    let label_id = "rushstack:feature/foo";
    let metadata = LabelMetadata {
        source_type: "label-metadata".to_string(),
        catalog: "rushstack".to_string(),
        label_id: label_id.to_string(),
        label: "feature/foo".to_string(),
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

#[test]
fn test_estimate_serialized_size_reasonable_estimate() {
    // Create a sample chunk with typical content
    let chunk = Chunk {
        text: "fn main() { println!(\"Hello, world!\"); }".repeat(10), // ~450 bytes
        source_uri: "src/main.rs".to_string(),
        source_type: "file".to_string(),
        catalog: "test-catalog".to_string(),
        content_hash: "abc123def456".to_string(),
        start_line: 1,
        end_line: 100,
        symbol_name: Some("main".to_string()),
        chunk_type: "function".to_string(),
        chunk_kind: "code".to_string(),
        breadcrumb: "main.rs:main".to_string(),
        label_id: "test-catalog:test-label".to_string(),
        active_label_ids: vec!["test-catalog:label1".to_string()],
        embedder_id: "test-embedder".to_string(),
        chunker_id: "test-chunker".to_string(),
        blob_id: "abc123".to_string(),
        file_id: "file123".to_string(),
        relative_path: "src/main.rs".to_string(),
        package_name: "test-package".to_string(),
        chunk_ordinal: 1,
        chunk_count: 1,
        split_part_ordinal: None,
        split_part_count: None,
    };

    // 384-dim embedding (typical for small models)
    let embedding = vec![0.0f32; 384];

    let estimated = QdrantUploader::estimate_serialized_size(&chunk, &embedding);

    // The heuristic should produce a reasonable estimate
    let text_len = chunk.text.len();
    let embedding_bytes = embedding.len() * 4;

    // Sanity check: estimate should be at least the sum of major components
    // (embedding is estimated as ~10 chars per number, not 4 bytes)
    let minimum_expected = text_len + embedding.len() * 8; // conservative lower bound
    assert!(
        estimated >= minimum_expected,
        "Estimated size {} is less than minimum expected {}",
        estimated,
        minimum_expected
    );

    // Sanity check: estimate should not be ridiculously large
    // (no more than 10x the text + embedding size)
    let maximum_expected = (text_len + embedding_bytes) * 10;
    assert!(
        estimated <= maximum_expected,
        "Estimated size {} is greater than maximum expected {}",
        estimated,
        maximum_expected
    );
}

#[test]
fn test_estimate_serialized_size_vs_actual_json() {
    // Create a sample chunk with realistic field sizes
    let chunk = Chunk {
        text: "fn main() { println!(\"Hello, world!\"); }".repeat(100), // ~4500 bytes
        source_uri: "libraries/rushstack/node-core-library/src/JsonFile.ts".to_string(),
        source_type: "file".to_string(),
        catalog: "rushstack".to_string(),
        content_hash: "a1b2c3d4e5f6789012345678901234567890abcd".to_string(),
        start_line: 1,
        end_line: 100,
        symbol_name: Some("JsonFile".to_string()),
        chunk_type: "function".to_string(),
        chunk_kind: "code".to_string(),
        breadcrumb: "@rushstack/node-core-library:JsonFile.ts:JsonFile.load".to_string(),
        label_id: "rushstack:main".to_string(),
        active_label_ids: vec!["rushstack:main".to_string(), "rushstack:pr-123".to_string()],
        embedder_id: "onnx-all-MiniLM-L6-v2".to_string(),
        chunker_id: "typescript-ast-partitioner".to_string(),
        blob_id: "abc123def456".to_string(),
        file_id: "1234567890abcdef".to_string(),
        relative_path: "libraries/rushstack/node-core-library/src/JsonFile.ts".to_string(),
        package_name: "@rushstack/node-core-library".to_string(),
        chunk_ordinal: 1,
        chunk_count: 5,
        split_part_ordinal: None,
        split_part_count: None,
    };

    // 384-dim embedding
    let embedding = vec![0.1f32; 384];

    // Get heuristic estimate
    let estimated = QdrantUploader::estimate_serialized_size(&chunk, &embedding);

    // Build actual Point and serialize to get real size
    let point = Point {
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
    };

    let actual_json = serde_json::to_vec(&point).expect("Failed to serialize point");
    let actual_size = actual_json.len();

    // The heuristic should be within 50% of the actual size
    // This is a reasonable tolerance for accumulation tracking purposes
    let lower_bound = actual_size as f64 * 0.5;
    let upper_bound = actual_size as f64 * 1.5;

    assert!(
        estimated >= lower_bound as usize,
        "Heuristic estimate {} is too low compared to actual {} (lower bound: {:.0})",
        estimated,
        actual_size,
        lower_bound
    );

    assert!(
        estimated <= upper_bound as usize,
        "Heuristic estimate {} is too high compared to actual {} (upper bound: {:.0})",
        estimated,
        actual_size,
        upper_bound
    );

    // Log the comparison for informational purposes
    eprintln!(
        "Size comparison: heuristic={}, actual={}, ratio={:.2}",
        estimated,
        actual_size,
        estimated as f64 / actual_size as f64
    );
}
