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
    
    /// Source file path
    pub file: String,
    
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
    
    /// Chunk type (e.g., "function", "class", "markdown-section", "json-key")
    pub chunk_type: String,
    
    /// Breadcrumb path (e.g., "@rushstack/node-core-library:JsonFile.ts:JsonFile.load")
    pub breadcrumb: String,
}

/// Chunks a file based on its type and content
/// 
/// # Arguments
/// 
/// * `file_path` - Path to the file to chunk
/// * `catalog` - Catalog name for this file
/// * `package_name` - Package name for breadcrumb (e.g., "@rushstack/node-core-library")
/// * `target_size` - Target chunk size in characters (default 1800)
/// 
/// # Returns
/// 
/// Vector of chunks or an error
pub fn chunk_file(
    file_path: &str, 
    catalog: &str, 
    package_name: &str,
    target_size: usize,
) -> Result<Vec<Chunk>> {
    let strategy = get_chunk_strategy(file_path);
    let content = fs::read_to_string(file_path)?;
    
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
            Ok(partitioned.into_iter().map(|p| Chunk::from(p)).collect())
        }
        ChunkingStrategy::JavaScript => {
            // Skip .js files for now (per todo plan)
            Ok(Vec::new())
        }
        ChunkingStrategy::Markdown => {
            // TODO: Implement heading-based splitting
            chunk_by_lines(file_path, catalog, &content, target_size, "markdown")
        }
        ChunkingStrategy::Json => {
            // Skip JSON files (low value for AI search)
            Ok(Vec::new())
        }
        ChunkingStrategy::YamlSimple => {
            chunk_by_lines(file_path, catalog, &content, target_size, "yaml")
        }
        ChunkingStrategy::SimpleLine => {
            chunk_by_lines(file_path, catalog, &content, target_size, "text")
        }
        ChunkingStrategy::Skip => {
            Ok(Vec::new())
        }
    }
}

impl From<PartitionedChunk> for Chunk {
    fn from(p: PartitionedChunk) -> Self {
        Chunk {
            text: p.text,
            file: p.file,
            catalog: p.catalog,
            content_hash: p.content_hash,
            start_line: p.start_line,
            end_line: p.end_line,
            symbol_name: p.symbol_name,
            chunk_type: p.chunk_type,
            breadcrumb: p.breadcrumb,
        }
    }
}

/// Simple line-based chunking for non-TypeScript files
fn chunk_by_lines(
    file_path: &str, 
    catalog: &str,
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
                file: file_path.to_string(),
                catalog: catalog.to_string(),
                content_hash: content_hash.clone(),
                start_line: start + 1,
                end_line: end,
                symbol_name: None,
                chunk_type: chunk_type.to_string(),
                breadcrumb: file_name.clone(),
            });
        }
        
        start = end;
    }

    Ok(chunks)
}
