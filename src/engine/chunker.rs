//! File chunking logic for different file types
//! 
//! This module handles splitting files into semantically meaningful chunks
//! based on their file type and content structure.

use anyhow::Result;
use std::fs;
use super::config::{ChunkingStrategy, get_chunk_strategy};

/// Represents a chunk of code or documentation
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The text content of the chunk
    pub text: String,
    
    /// Source file path
    pub file: String,
    
    /// Starting line number (1-indexed)
    pub start_line: usize,
    
    /// Ending line number (inclusive)
    pub end_line: usize,
    
    /// Optional symbol name (for functions, classes, etc.)
    pub symbol_name: Option<String>,
    
    /// Chunk type (e.g., "function", "class", "markdown-section", "json-key")
    pub chunk_type: String,
}

/// Chunks a file based on its type and content
/// 
/// # Arguments
/// 
/// * `file_path` - Path to the file to chunk
/// * `max_lines` - Maximum lines per chunk (for line-based strategies)
/// 
/// # Returns
/// 
/// Vector of chunks or an error
pub fn chunk_file(file_path: &str, max_lines: usize) -> Result<Vec<Chunk>> {
    let strategy = get_chunk_strategy(file_path);
    
    match strategy {
        ChunkingStrategy::TypeScript | ChunkingStrategy::JavaScript => {
            // Simple line-based chunking (placeholder - will improve with tree-sitter later)
            chunk_by_lines(file_path, max_lines, "code")
        }
        ChunkingStrategy::Markdown => {
            // Simple line-based chunking (placeholder - will improve with heading-based splitting later)
            chunk_by_lines(file_path, max_lines, "markdown")
        }
        ChunkingStrategy::Json => {
            // Simple line-based chunking (placeholder - will improve with 2-level key splitting later)
            chunk_by_lines(file_path, max_lines, "json")
        }
        ChunkingStrategy::YamlSimple => {
            chunk_by_lines(file_path, max_lines, "yaml")
        }
        ChunkingStrategy::SimpleLine => {
            chunk_by_lines(file_path, max_lines, "text")
        }
        ChunkingStrategy::Skip => {
            Ok(Vec::new())
        }
    }
}

/// Simple line-based chunking
fn chunk_by_lines(file_path: &str, max_lines: usize, chunk_type: &str) -> Result<Vec<Chunk>> {
    let content = fs::read_to_string(file_path)?;
    let lines: Vec<&str> = content.lines().collect();
    
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < lines.len() {
        let end = (start + max_lines).min(lines.len());
        let chunk_text = lines[start..end].join("\n");
        
        // Skip empty or whitespace-only chunks
        if !chunk_text.trim().is_empty() {
            chunks.push(Chunk {
                text: chunk_text,
                file: file_path.to_string(),
                start_line: start + 1,
                end_line: end,
                symbol_name: None,
                chunk_type: chunk_type.to_string(),
            });
        }
        
        start = end;
    }

    Ok(chunks)
}
