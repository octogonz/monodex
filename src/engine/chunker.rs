//! File chunking logic for different file types
//! 
//! This module handles splitting files into semantically meaningful chunks
//! based on their file type and content structure.

use anyhow::Result;
use std::fs;
use tree_sitter::{Node, Parser};
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
/// * `max_lines` - Maximum lines per chunk (for fallback line-based chunking)
/// 
/// # Returns
/// 
/// Vector of chunks or an error
pub fn chunk_file(file_path: &str, max_lines: usize) -> Result<Vec<Chunk>> {
    let strategy = get_chunk_strategy(file_path);
    
    match strategy {
        ChunkingStrategy::TypeScript => {
            chunk_typescript(file_path, max_lines)
        }
        ChunkingStrategy::JavaScript => {
            // Skip .js files for now (per todo plan)
            Ok(Vec::new())
        }
        ChunkingStrategy::Markdown => {
            // TODO: Implement heading-based splitting
            chunk_by_lines(file_path, max_lines, "markdown")
        }
        ChunkingStrategy::Json => {
            // TODO: Implement schema-based splitting
            Ok(Vec::new())
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

/// Chunk TypeScript/TSX files using tree-sitter AST parsing
/// 
/// Extracts semantic units:
/// - Functions (function declarations, arrow functions, method definitions)
/// - Classes (class declarations)
/// - Interfaces (interface declarations)
/// - Type aliases (type alias declarations)
/// - Enums (enum declarations)
/// - Variables with initializers (const/let with function values)
fn chunk_typescript(file_path: &str, max_lines: usize) -> Result<Vec<Chunk>> {
    let content = fs::read_to_string(file_path)?;
    let source = content.as_bytes();

    // Initialize tree-sitter parser with TypeScript grammar
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_typescript::language_typescript())
        .map_err(|e| anyhow::anyhow!("Failed to set TypeScript language: {}", e))?;

    let tree = parser.parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse TypeScript file"))?;

    let root = tree.root_node();
    let mut chunks = Vec::new();

    // Extract semantic units from the AST
    extract_semantic_units(&root, source, file_path, &mut chunks);

    // If no semantic units found, fall back to line-based chunking
    if chunks.is_empty() {
        return chunk_by_lines(file_path, max_lines, "code");
    }

    Ok(chunks)
}

/// Extract semantic units from TypeScript AST
fn extract_semantic_units<'a>(
    node: &Node<'a>,
    source: &[u8],
    file_path: &str,
    chunks: &mut Vec<Chunk>,
) {
    let node_kind = node.kind();

    match node_kind {
        // Function declarations
        "function_declaration" => {
            if let Some(chunk) = create_chunk_from_node(node, source, file_path, "function") {
                chunks.push(chunk);
            }
        }
        
        // Class declarations
        "class_declaration" => {
            // Extract the whole class
            if let Some(chunk) = create_chunk_from_node(node, source, file_path, "class") {
                chunks.push(chunk);
            }
            
            // Also extract individual methods for more granular search
            for child in node.children(&mut node.walk()) {
                if child.kind() == "class_body" {
                    for method in child.children(&mut child.walk()) {
                        if method.kind() == "method_definition" {
                            if let Some(chunk) = create_chunk_from_node(&method, source, file_path, "method") {
                                chunks.push(chunk);
                            }
                        }
                    }
                }
            }
        }
        
        // Interface declarations
        "interface_declaration" => {
            if let Some(chunk) = create_chunk_from_node(node, source, file_path, "interface") {
                chunks.push(chunk);
            }
        }
        
        // Type alias declarations
        "type_alias_declaration" => {
            if let Some(chunk) = create_chunk_from_node(node, source, file_path, "type") {
                chunks.push(chunk);
            }
        }
        
        // Enum declarations
        "enum_declaration" => {
            if let Some(chunk) = create_chunk_from_node(node, source, file_path, "enum") {
                chunks.push(chunk);
            }
        }
        
        // Export statements - recurse into them
        "export_statement" => {
            for child in node.children(&mut node.walk()) {
                extract_semantic_units(&child, source, file_path, chunks);
            }
        }
        
        // Variable declarations with function values
        "variable_declaration" | "lexical_declaration" => {
            // Check if any variable is initialized with a function
            for child in node.children(&mut node.walk()) {
                if child.kind() == "variable_declarator" {
                    if let Some(value_node) = child.child_by_field_name("value") {
                        if value_node.kind() == "arrow_function" || value_node.kind() == "function_expression" {
                            if let Some(chunk) = create_chunk_from_node(node, source, file_path, "function") {
                                chunks.push(chunk);
                            }
                            break;
                        }
                    }
                }
            }
        }
        
        // Ambient declarations (declare module, declare function, etc.)
        "ambient_declaration" => {
            for child in node.children(&mut node.walk()) {
                if matches!(child.kind(), "module" | "function_declaration" | "variable_declaration") {
                    if let Some(chunk) = create_chunk_from_node(node, source, file_path, "ambient") {
                        chunks.push(chunk);
                    }
                }
            }
        }
        
        _ => {}
    }

    // Recurse into children for nested declarations
    for child in node.children(&mut node.walk()) {
        // Skip recursing into class bodies (we already handled methods)
        if child.kind() != "class_body" {
            extract_semantic_units(&child, source, file_path, chunks);
        }
    }
}

/// Create a Chunk from a tree-sitter node
/// 
/// Includes preceding JSDoc/TSDoc comments if they exist.
fn create_chunk_from_node(node: &Node, source: &[u8], file_path: &str, chunk_type: &str) -> Option<Chunk> {
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    
    if end_byte <= start_byte {
        return None;
    }

    // Look for preceding JSDoc/TSDoc comments
    let (start_with_comments, start_line) = find_including_preceding_comment(node, source);

    let text = String::from_utf8_lossy(&source[start_with_comments..end_byte]).to_string();
    
    // Skip very small chunks (less than 50 characters)
    if text.len() < 50 {
        return None;
    }

    // Extract symbol name if available
    let symbol_name = node.child_by_field_name("name")
        .map(|name_node| {
            String::from_utf8_lossy(&source[name_node.start_byte()..name_node.end_byte()]).to_string()
        });

    Some(Chunk {
        text,
        file: file_path.to_string(),
        start_line,
        end_line: node.end_position().row + 1,
        symbol_name,
        chunk_type: chunk_type.to_string(),
    })
}

/// Find the start position including any preceding JSDoc/TSDoc comments
/// 
/// JSDoc comments are comment nodes that immediately precede the target node.
/// They are siblings in the AST, not children.
fn find_including_preceding_comment(node: &Node, source: &[u8]) -> (usize, usize) {
    let start = node.start_byte();
    let start_line = node.start_position().row + 1;

    // Get parent to find the node and its previous siblings
    if let Some(parent) = node.parent() {
        let mut prev_was_comment = false;
        
        // Iterate through parent's children
        for child in parent.children(&mut parent.walk()) {
            if child == *node {
                // We found the target node
                // If the previous sibling was a JSDoc comment, include it
                if prev_was_comment {
                    // Actually, we need to find the specific previous sibling
                    // This is getting complex - for now, let's try a different approach
                    // by scanning backwards in the source
                    return find_comment_backwards(node, source);
                }
                break;
            }
            prev_was_comment = is_jsdoc_comment(&child, source);
        }
    }

    (start, start_line)
}

/// Scan backwards from the node to find JSDoc comments
fn find_comment_backwards(node: &Node, source: &[u8]) -> (usize, usize) {
    let mut pos = node.start_byte();
    let start_line = node.start_position().row + 1;
    
    // Look backwards for /** patterns
    while pos > 3 {
        // Check if we found the start of a JSDoc comment
        if source[pos - 3] == b'/' && source[pos - 2] == b'*' && source[pos - 1] == b'*' {
            // Found /**, now find the start of the line
            while pos > 0 && source[pos - 1] != b'\n' {
                pos -= 1;
            }
            return (pos, start_line);
        }
        pos -= 1;
        
        // Limit search to reasonable distance (e.g., 5 lines)
        let max_distance = 500; // 500 chars back
        if node.start_byte() - pos > max_distance {
            break;
        }
    }
    
    (node.start_byte(), start_line)
}

/// Check if a node is a JSDoc/TSDoc comment
fn is_jsdoc_comment(node: &Node, source: &[u8]) -> bool {
    // Tree-sitter TypeScript grammar identifies comments as "comment" nodes
    // JSDoc comments start with /**
    if node.kind() == "comment" {
        let text = String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]);
        return text.trim_start().starts_with("/**");
    }
    
    false
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
