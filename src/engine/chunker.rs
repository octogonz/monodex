//! File chunking logic for different file types
//! 
//! This module handles splitting files into semantically meaningful chunks
//! based on their file type and content structure.

use anyhow::Result;
use std::fs;
use super::config::{ChunkingStrategy, get_chunk_strategy};
use super::partitioner::{partition_typescript, PartitionConfig, PartitionedChunk};
use super::util::compute_hash;

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
    
    /// File ID (stable hash of relative path from catalog base)
    pub file_id: u64,
    
    /// Relative path from catalog base (e.g., "libraries/rush-lib/src/JsonFile.ts")
    pub relative_path: String,
    
    /// Chunk number within file (1-indexed, ordered by start_line)
    pub chunk_number: usize,
    
    /// Total number of chunks in this file
    pub chunk_count: usize,
}

/// Chunks a file based on its type and content
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
pub fn chunk_file(
    file_path: &str, 
    catalog: &str, 
    catalog_base_path: &str,
    package_name: &str,
    target_size: usize,
) -> Result<Vec<Chunk>> {
    let strategy = get_chunk_strategy(file_path);
    let content = fs::read_to_string(file_path)?;
    
    // Compute relative path from catalog base
    let relative_path = file_path
        .strip_prefix(catalog_base_path)
        .unwrap_or(file_path)
        .trim_start_matches('/')
        .to_string();
    
    // Compute file ID from relative path
    let file_id = super::util::compute_file_id(&relative_path);
    
    match strategy {
        ChunkingStrategy::TypeScript => {
            let file_name = std::path::Path::new(file_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| file_path.to_string());
            
            let config = PartitionConfig {
                target_size,
                file_name,
                package_name: package_name.to_string(),
                ..Default::default()
            };
            
            let partitioned = partition_typescript(&content, &config, file_path, catalog);
            let mut chunks: Vec<Chunk> = partitioned.into_iter().map(|p| {
                let mut chunk = Chunk::from(p);
                chunk.file_id = file_id;
                chunk.relative_path = relative_path.clone();
                chunk
            }).collect();
            
            // Assign chunk numbers (1-indexed, sorted by start_line)
            chunks.sort_by_key(|c| c.start_line);
            let chunk_count = chunks.len();
            for (i, chunk) in chunks.iter_mut().enumerate() {
                chunk.chunk_number = i + 1;
                chunk.chunk_count = chunk_count;
            }
            
            Ok(chunks)
        }
        ChunkingStrategy::JavaScript => {
            // Skip .js files for now (per todo plan)
            Ok(Vec::new())
        }
        ChunkingStrategy::Markdown => {
            // TODO: Implement heading-based splitting
            chunk_by_lines(file_path, catalog, &relative_path, file_id, &content, target_size, "markdown")
        }
        ChunkingStrategy::Json => {
            // Skip JSON files (low value for AI search)
            Ok(Vec::new())
        }
        ChunkingStrategy::Skip => Ok(Vec::new()),
        ChunkingStrategy::YamlSimple => {
            chunk_by_lines(file_path, catalog, &relative_path, file_id, &content, target_size, "yaml")
        }
        ChunkingStrategy::SimpleLine => {
            chunk_by_lines(file_path, catalog, &relative_path, file_id, &content, target_size, "text")
        }
    }
}

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
            // These fields are set by chunk_file after conversion
            file_id: 0,
            relative_path: String::new(),
            chunk_number: 0,
            chunk_count: 0,
        }
    }
}

/// Chunk by lines for simple text files
fn chunk_by_lines(
    file_path: &str, 
    catalog: &str, 
    relative_path: &str,
    file_id: u64,
    content: &str, 
    max_chars: usize, 
    chunk_type: &str,
) -> Result<Vec<Chunk>> {
    let content_hash = compute_hash(content);
    let lines: Vec<&str> = content.lines().collect();
    
    let mut chunks = Vec::new();
    let mut start = 0;
    let file_name = std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| file_path.to_string());

    while start < lines.len() {
        let mut end = start;
        let mut size = 0;
        
        // Build chunk up to max_chars
        while end < lines.len() && size + lines[end].len() + 1 <= max_chars {
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
                source_uri: file_path.to_string(),
                source_type: "code".to_string(),
                catalog: catalog.to_string(),
                content_hash: content_hash.clone(),
                start_line: start + 1,
                end_line: end,
                symbol_name: None,
                chunk_type: chunk_type.to_string(),
                chunk_kind: "content".to_string(),
                breadcrumb: file_name.clone(),
                file_id,
                relative_path: relative_path.to_string(),
                chunk_number: 0, // Will update after loop
                chunk_count: 0,  // Will update after loop
            });
        }
        
        start = end;
    }
    
    // Update chunk_number and chunk_count for all chunks
    let total_chunks = chunks.len().max(1);
    for (i, chunk) in chunks.iter_mut().enumerate() {
        chunk.chunk_number = i + 1;
        chunk.chunk_count = total_chunks;
    }

    Ok(chunks)
}
