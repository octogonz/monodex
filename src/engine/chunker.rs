//! File chunking logic for different file types
//!
//! This module handles splitting files into semantically meaningful chunks
//! based on their file type and content structure.

use super::config::{ChunkingStrategy, get_chunk_strategy};
use super::partitioner::{PartitionConfig, PartitionedChunk, partition_typescript};
use super::util::{CHUNKER_ID, EMBEDDER_ID, compute_file_id, compute_hash, compute_point_id};
use anyhow::Result;
use std::fs;

/// Represents a chunk of code or documentation
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The text content of the chunk
    pub text: String,

    /// Source URI (full file path, issue reference, etc.)
    pub source_uri: String,

    /// Source type (e.g., "code", "issue", "discussion", "document")
    pub source_type: String,

    /// Catalog name (for multi-source partitioning)
    pub catalog: String,

    /// Content hash (SHA256) for incremental sync
    pub content_hash: String,

    /// Starting line number (1-indexed)
    pub start_line: usize,

    /// Ending line number (inclusive)
    pub end_line: usize,

    /// Optional symbol name (for functions, classes, etc.)
    pub symbol_name: Option<String>,

    /// Chunk type (e.g., "function", "class", "markdown-section", "issue-comment")
    pub chunk_type: String,

    /// Chunk kind (content, imports, changelog, config)
    pub chunk_kind: String,

    /// Breadcrumb path (e.g., "@rushstack/node-core-library:JsonFile.ts:JsonFile.load")
    pub breadcrumb: String,

    // --- Phase 2: Label-aware indexing fields ---
    /// The initiating label for this chunk (transitional)
    pub label_id: String,

    /// All labels this chunk belongs to (authoritative)
    pub active_label_ids: Vec<String>,

    /// Implementation identifier for the embedder
    pub embedder_id: String,

    /// Implementation identifier for the chunker
    pub chunker_id: String,

    /// Git blob SHA (content provenance)
    pub blob_id: String,

    /// Package name for breadcrumb (e.g., "@rushstack/node-core-library")
    pub package_name: String,

    /// File ID - semantic file identity (16-char hex string)
    pub file_id: String,

    /// Relative path from catalog base (e.g., "libraries/rush-lib/src/JsonFile.ts")
    pub relative_path: String,

    /// Chunk ordinal within file (1-indexed, ordered by start_line)
    pub chunk_ordinal: usize,

    /// Total number of chunks in this file
    pub chunk_count: usize,
}

impl Chunk {
    /// Compute the point ID for this chunk
    pub fn point_id(&self) -> String {
        compute_point_id(&self.file_id, self.chunk_ordinal)
    }
}

/// Context needed for chunking with Phase 2 schema
pub struct ChunkContext {
    /// Catalog name
    pub catalog: String,
    /// Label ID (e.g., "rushstack:main")
    pub label_id: String,
    /// Package name for breadcrumb
    pub package_name: String,
    /// Relative path from catalog base
    pub relative_path: String,
    /// Git blob SHA
    pub blob_id: String,
    /// Source URI (full path for display)
    pub source_uri: String,
}

/// Chunks file content based on its type
///
/// This is the new content-based chunking API for Phase 2.
///
/// # Arguments
///
/// * `content` - File content as string
/// * `ctx` - Chunk context with identity information
/// * `target_size` - Target chunk size in characters (default 6000)
///
/// # Returns
///
/// Vector of chunks or an error
pub fn chunk_content(content: &str, ctx: &ChunkContext, target_size: usize) -> Result<Vec<Chunk>> {
    let strategy = get_chunk_strategy(&ctx.relative_path);

    // Compute file ID from the new identity components
    let file_id = compute_file_id(EMBEDDER_ID, CHUNKER_ID, &ctx.blob_id, &ctx.relative_path);

    match strategy {
        ChunkingStrategy::TypeScript => {
            let file_name = std::path::Path::new(&ctx.relative_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| ctx.relative_path.to_string());

            let config = PartitionConfig {
                target_size,
                file_name,
                package_name: ctx.package_name.clone(),
                ..Default::default()
            };

            let partitioned = partition_typescript(content, &config, &ctx.source_uri, &ctx.catalog);
            let mut chunks: Vec<Chunk> = partitioned
                .into_iter()
                .enumerate()
                .map(|(i, p)| {
                    Chunk::from_partitioned(p, &file_id, &ctx, i + 1, 0) // chunk_count set later
                })
                .collect();

            // Assign chunk ordinals (1-indexed, sorted by start_line)
            chunks.sort_by_key(|c| c.start_line);
            let chunk_count = chunks.len();
            for (i, chunk) in chunks.iter_mut().enumerate() {
                chunk.chunk_ordinal = i + 1;
                chunk.chunk_count = chunk_count;
            }

            Ok(chunks)
        }
        ChunkingStrategy::Markdown => {
            // TODO: Implement heading-based splitting
            chunk_by_lines(content, &file_id, ctx, target_size, "markdown")
        }
        ChunkingStrategy::LineBased => chunk_by_lines(content, &file_id, ctx, target_size, "text"),
        ChunkingStrategy::Skip => Ok(Vec::new()),
    }
}

impl Chunk {
    /// Create a chunk from a PartitionedChunk with Phase 2 fields
    fn from_partitioned(
        p: PartitionedChunk,
        file_id: &str,
        ctx: &ChunkContext,
        chunk_ordinal: usize,
        chunk_count: usize,
    ) -> Self {
        Chunk {
            text: p.text,
            source_uri: ctx.source_uri.clone(),
            source_type: "code".to_string(),
            catalog: ctx.catalog.clone(),
            content_hash: p.content_hash,
            start_line: p.start_line,
            end_line: p.end_line,
            symbol_name: p.symbol_name,
            chunk_type: p.chunk_type,
            chunk_kind: p.chunk_kind,
            breadcrumb: p.breadcrumb,
            // Phase 2 fields
            label_id: ctx.label_id.clone(),
            active_label_ids: vec![ctx.label_id.clone()],
            embedder_id: EMBEDDER_ID.to_string(),
            chunker_id: CHUNKER_ID.to_string(),
            blob_id: ctx.blob_id.clone(),
            package_name: ctx.package_name.clone(),
            file_id: file_id.to_string(),
            relative_path: ctx.relative_path.clone(),
            chunk_ordinal,
            chunk_count,
        }
    }
}

// Legacy support: Implement From<PartitionedChunk> for backwards compatibility
// during migration. Most fields need to be filled in later.
impl From<PartitionedChunk> for Chunk {
    fn from(p: PartitionedChunk) -> Self {
        Chunk {
            text: p.text,
            source_uri: p.source_uri,
            source_type: "code".to_string(),
            catalog: p.catalog,
            content_hash: p.content_hash,
            start_line: p.start_line,
            end_line: p.end_line,
            symbol_name: p.symbol_name,
            chunk_type: p.chunk_type,
            chunk_kind: p.chunk_kind,
            breadcrumb: p.breadcrumb,
            // Phase 2 fields - must be filled in by caller
            label_id: String::new(),
            active_label_ids: Vec::new(),
            embedder_id: EMBEDDER_ID.to_string(),
            chunker_id: CHUNKER_ID.to_string(),
            blob_id: String::new(),
            package_name: String::new(),
            file_id: String::new(),
            relative_path: String::new(),
            chunk_ordinal: 0,
            chunk_count: 0,
        }
    }
}

/// Chunk by lines for simple text files
fn chunk_by_lines(
    content: &str,
    file_id: &str,
    ctx: &ChunkContext,
    max_chars: usize,
    chunk_type: &str,
) -> Result<Vec<Chunk>> {
    let content_hash = compute_hash(content);
    let lines: Vec<&str> = content.lines().collect();

    let mut chunks = Vec::new();
    let mut start = 0;
    let file_name = std::path::Path::new(&ctx.relative_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| ctx.relative_path.to_string());

    while start < lines.len() {
        let mut end = start;
        let mut size = 0;

        // Build chunk up to max_chars
        while end < lines.len() && size + lines[end].len() < max_chars {
            size += lines[end].len() + 1;
            end += 1;
        }

        // Ensure at least one line per chunk
        if end == start && start < lines.len() {
            end = start + 1;
        }

        let chunk_text = lines[start..end].join("\n");

        // Skip empty or whitespace-only chunks
        if !chunk_text.trim().is_empty() {
            chunks.push(Chunk {
                text: chunk_text,
                source_uri: ctx.source_uri.clone(),
                source_type: "code".to_string(),
                catalog: ctx.catalog.clone(),
                content_hash: content_hash.clone(),
                start_line: start + 1,
                end_line: end,
                symbol_name: None,
                chunk_type: chunk_type.to_string(),
                chunk_kind: "content".to_string(),
                breadcrumb: file_name.clone(),
                // Phase 2 fields
                label_id: ctx.label_id.clone(),
                active_label_ids: vec![ctx.label_id.clone()],
                embedder_id: EMBEDDER_ID.to_string(),
                chunker_id: CHUNKER_ID.to_string(),
                blob_id: ctx.blob_id.clone(),
                package_name: ctx.package_name.clone(),
                file_id: file_id.to_string(),
                relative_path: ctx.relative_path.clone(),
                chunk_ordinal: 0, // Will update after loop
                chunk_count: 0,   // Will update after loop
            });
        }

        start = end;
    }

    // Update chunk_ordinal and chunk_count for all chunks
    let total_chunks = chunks.len().max(1);
    for (i, chunk) in chunks.iter_mut().enumerate() {
        chunk.chunk_ordinal = i + 1;
        chunk.chunk_count = total_chunks;
    }

    Ok(chunks)
}

// ========================================
// Legacy filesystem-based chunking API
// ========================================

/// Chunks a file based on its type and content (legacy filesystem API)
///
/// # Arguments
///
/// * `file_path` - Path to the file to chunk
/// * `catalog` - Catalog name for this file
/// * `catalog_base_path` - Base path of the catalog (for computing relative paths)
/// * `package_name` - Package name for breadcrumb (e.g., "@rushstack/node-core-library")
/// * `target_size` - Target chunk size in characters (default 6000)
///
/// # Returns
///
/// Vector of chunks or an error
///
/// Note: This legacy API produces chunks with empty label_id and blob_id.
/// Use `chunk_content` for the new Phase 2 API.
#[allow(dead_code)]
pub fn chunk_file(
    file_path: &str,
    catalog: &str,
    catalog_base_path: &str,
    package_name: &str,
    target_size: usize,
) -> Result<Vec<Chunk>> {
    let content = fs::read_to_string(file_path)?;

    // Compute relative path from catalog base
    let relative_path = file_path
        .strip_prefix(catalog_base_path)
        .unwrap_or(file_path)
        .trim_start_matches('/')
        .to_string();

    let ctx = ChunkContext {
        catalog: catalog.to_string(),
        label_id: String::new(), // Legacy: no label
        package_name: package_name.to_string(),
        relative_path,
        blob_id: String::new(), // Legacy: no blob_id
        source_uri: file_path.to_string(),
    };

    chunk_content(&content, &ctx, target_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a test chunk context
    fn test_context(blob_id: &str, relative_path: &str, package_name: &str) -> ChunkContext {
        ChunkContext {
            catalog: "test-catalog".to_string(),
            label_id: "test-catalog:main".to_string(),
            package_name: package_name.to_string(),
            relative_path: relative_path.to_string(),
            blob_id: blob_id.to_string(),
            source_uri: format!("/repo/{}", relative_path),
        }
    }

    /// Test that same content + path produces same file_id
    #[test]
    fn test_same_content_path_produces_same_file_id() {
        let content = r#"
export function hello() {
    console.log("Hello, world!");
}
"#;
        let ctx = test_context("abc123", "src/index.ts", "@test/pkg");

        let chunks1 = chunk_content(content, &ctx, 6000).unwrap();
        let chunks2 = chunk_content(content, &ctx, 6000).unwrap();

        assert_eq!(chunks1.len(), chunks2.len());
        for (c1, c2) in chunks1.iter().zip(chunks2.iter()) {
            assert_eq!(
                c1.file_id, c2.file_id,
                "Same content+path should produce same file_id"
            );
            assert_eq!(
                c1.point_id(),
                c2.point_id(),
                "Same content+path should produce same point_id"
            );
        }
    }

    /// Test that path changes produce different file_id (expected behavior)
    /// Path is part of semantic identity because it affects breadcrumb context
    #[test]
    fn test_path_change_produces_different_file_id() {
        let content = r#"
export function hello() {
    console.log("Hello, world!");
}
"#;
        let ctx1 = test_context("abc123", "src/index.ts", "@test/pkg");
        let ctx2 = test_context("abc123", "lib/index.ts", "@test/pkg");

        let chunks1 = chunk_content(content, &ctx1, 6000).unwrap();
        let chunks2 = chunk_content(content, &ctx2, 6000).unwrap();

        assert!(!chunks1.is_empty() && !chunks2.is_empty());
        assert_ne!(
            chunks1[0].file_id, chunks2[0].file_id,
            "Different paths should produce different file_id"
        );
        assert_ne!(
            chunks1[0].point_id(),
            chunks2[0].point_id(),
            "Different paths should produce different point_id"
        );
    }

    /// Test that same content at different paths = different chunks (semantic context matters)
    /// This verifies that path renames create new chunks even if content is identical
    #[test]
    fn test_content_at_different_paths_creates_different_chunks() {
        let content = r#"
export class JsonFile {
    public static load(path: string): object {
        return JSON.parse(fs.readFileSync(path, 'utf-8'));
    }
}
"#;
        // Simulate a file moving from libraries/foo to libraries/bar
        let ctx1 = test_context("abc123", "libraries/foo/src/JsonFile.ts", "@scope/foo");
        let ctx2 = test_context("abc123", "libraries/bar/src/JsonFile.ts", "@scope/bar");

        let chunks1 = chunk_content(content, &ctx1, 6000).unwrap();
        let chunks2 = chunk_content(content, &ctx2, 6000).unwrap();

        // Both should produce chunks
        assert!(!chunks1.is_empty() && !chunks2.is_empty());

        // File IDs should be different (path is part of identity)
        assert_ne!(chunks1[0].file_id, chunks2[0].file_id);

        // Point IDs should be different
        assert_ne!(chunks1[0].point_id(), chunks2[0].point_id());

        // Breadcrumbs should reflect the different package context
        assert!(
            chunks1[0].breadcrumb.starts_with("@scope/foo"),
            "Breadcrumb should start with @scope/foo, got: {}",
            chunks1[0].breadcrumb
        );
        assert!(
            chunks2[0].breadcrumb.starts_with("@scope/bar"),
            "Breadcrumb should start with @scope/bar, got: {}",
            chunks2[0].breadcrumb
        );
    }

    /// Test that blob_id changes produce different file_id
    #[test]
    fn test_content_change_produces_different_file_id() {
        let content = r#"
export function hello() {
    console.log("Hello, world!");
}
"#;
        // Same path, different blob_id (different content)
        let ctx1 = test_context("abc123", "src/index.ts", "@test/pkg");
        let ctx2 = test_context("def456", "src/index.ts", "@test/pkg");

        let chunks1 = chunk_content(content, &ctx1, 6000).unwrap();
        let chunks2 = chunk_content(content, &ctx2, 6000).unwrap();

        assert!(!chunks1.is_empty() && !chunks2.is_empty());
        assert_ne!(
            chunks1[0].file_id, chunks2[0].file_id,
            "Different blob_id should produce different file_id"
        );
    }

    /// Test chunk ordinals are assigned correctly
    #[test]
    fn test_chunk_ordinals_assigned_correctly() {
        // Create a file large enough to be split into multiple chunks
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!(
                r#"
export function function_{}() {{
    console.log("Function {}");
    // This is a long comment to increase the size of this function
    // Adding more lines to make it larger
    // And even more lines to ensure it exceeds the target size
    let x = {};
    let y = {};
    let z = x + y;
    return z;
}}
"#,
                i,
                i,
                i * 10,
                i * 20
            ));
        }

        let ctx = test_context("abc123", "src/large.ts", "@test/pkg");
        let chunks = chunk_content(&content, &ctx, 1000).unwrap(); // Small target to force splits

        // Should have multiple chunks
        assert!(
            chunks.len() > 1,
            "Expected multiple chunks, got {}",
            chunks.len()
        );

        // Check ordinals are sequential starting from 1
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(
                chunk.chunk_ordinal,
                i + 1,
                "Chunk ordinal should be {}",
                i + 1
            );
        }

        // All chunks should have the same chunk_count
        let expected_count = chunks.len();
        for chunk in &chunks {
            assert_eq!(chunk.chunk_count, expected_count);
        }

        // Chunks should have non-empty file_id
        for chunk in &chunks {
            assert!(!chunk.file_id.is_empty());
            assert_eq!(chunk.file_id.len(), 16, "file_id should be 16 hex chars");
        }
    }
}
