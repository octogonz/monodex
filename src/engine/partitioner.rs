//! Partition-based code chunking
//!
//! This module implements a partition algorithm that recursively splits the AST
//! into chunks of roughly equal size, where each chunk includes breadcrumb context.

use tree_sitter::{Node, Parser};
use super::util::compute_hash;

/// Configuration for partition chunking
pub struct PartitionConfig {
    /// Target chunk size in characters (text only, breadcrumb is extra)
    pub target_size: usize,
    
    /// Maximum breadcrumb depth (file > class > method > ...)
    pub max_breadcrumb_depth: usize,
    
    /// File name for breadcrumb prefix
    pub file_name: String,
    
    /// Package name for breadcrumb (e.g., "@rushstack/node-core-library")
    pub package_name: String,
}

impl Default for PartitionConfig {
    fn default() -> Self {
        Self {
            target_size: 1800,
            max_breadcrumb_depth: 4,
            file_name: "unknown.ts".to_string(),
            package_name: "unknown".to_string(),
        }
    }
}

/// A chunk of code with breadcrumb context
#[derive(Debug, Clone)]
pub struct PartitionedChunk {
    /// Source file path
    pub file: String,
    
    /// Catalog name (for multi-source partitioning)
    pub catalog: String,
    
    /// Content hash (SHA256) for incremental sync
    pub content_hash: String,
    
    /// Breadcrumb path (e.g., "@rushstack/node-core-library:JsonFile.ts:JsonFile.load")
    pub breadcrumb: String,
    
    /// Source code text (including preceding comments)
    pub text: String,
    
    /// Starting line number (1-indexed)
    pub start_line: usize,
    
    /// Ending line number (inclusive)
    pub end_line: usize,
    
    /// Chunk type (function, class, method, etc.)
    pub chunk_type: String,
    
    /// Symbol name (if applicable)
    pub symbol_name: Option<String>,
}

/// Partition a TypeScript/TSX file into chunks
pub fn partition_typescript(
    source: &str, 
    config: &PartitionConfig,
    file_path: &str,
    catalog: &str,
) -> Vec<PartitionedChunk> {
    let content_hash = compute_hash(source);
    
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_typescript::language_typescript())
        .expect("Failed to set TypeScript language");
    
    let tree = parser.parse(source, None)
        .expect("Failed to parse TypeScript");
    
    let root = tree.root_node();
    let mut chunks = Vec::new();
    
    // Build base breadcrumb: package:file
    let base_breadcrumb = if config.package_name.is_empty() {
        config.file_name.clone()
    } else {
        format!("{}:{}", config.package_name, config.file_name)
    };
    
    partition_node(
        root,
        source.as_bytes(),
        config,
        &base_breadcrumb,
        "",  // class_context (will be detected)
        0,  // depth
        file_path,
        catalog,
        &content_hash,
        &mut chunks,
    );
    
    chunks
}

/// Recursively partition a node into chunks
fn partition_node(
    node: Node,
    source: &[u8],
    config: &PartitionConfig,
    breadcrumb_prefix: &str,
    class_context: &str,
    depth: usize,
    file_path: &str,
    catalog: &str,
    content_hash: &str,
    chunks: &mut Vec<PartitionedChunk>,
) {
    // Build the full text for this node (including preceding comments)
    let (text, start_line) = extract_text_with_preceding_comments(node, source);
    let end_line = node.end_position().row + 1;
    
    // Determine class context for this node
    let new_class_context = if get_chunk_type(node) == "class" {
        get_symbol_name(node, source).unwrap_or_else(|| class_context.to_string())
    } else {
        class_context.to_string()
    };
    
    // Build breadcrumb
    let breadcrumb = build_breadcrumb(node, source, breadcrumb_prefix, &new_class_context, depth);
    
    // Total size = breadcrumb + text
    let total_size = breadcrumb.len() + text.len();
    
    // Check if we should stop splitting (but still split oversized at any depth)
    if total_size <= config.target_size {
        // This node fits in budget - create a chunk
        if text.len() >= 50 {  // Minimum meaningful chunk size
            let chunk_type = get_chunk_type(node);
            let symbol_name = get_symbol_name(node, source);
            
            chunks.push(PartitionedChunk {
                file: file_path.to_string(),
                catalog: catalog.to_string(),
                content_hash: content_hash.to_string(),
                breadcrumb,
                text,
                start_line,
                end_line,
                chunk_type,
                symbol_name,
            });
        }
        return;
    }
    
    // If we're at max depth but still oversized, we still need to split
    if depth >= config.max_breadcrumb_depth {
        split_oversized_leaf(node, source, config, &breadcrumb, start_line, file_path, catalog, content_hash, chunks);
        return;
    }
    
    // Too big - need to split
    // Get children that can be partitioned
    let partitionable_children = get_partitionable_children(node);
    
    // Check if this is a function/method/class with a body that we should split
    let is_function_like = matches!(node.kind(), "function_declaration" | "method_definition" | "arrow_function");
    
    if partitionable_children.is_empty() || (is_function_like && text.len() > config.target_size) {
        // Leaf node OR oversized function/method - try AST-based splitting
        split_oversized_leaf(node, source, config, &breadcrumb, start_line, file_path, catalog, content_hash, chunks);
        return;
    }
    
    // If we have children to partition
    // Calculate cumulative text sizes
    let child_sizes: Vec<(Node, usize, usize)> = partitionable_children
        .iter()
        .map(|child| {
            let child_text = extract_text_with_preceding_comments(*child, source).0;
            (*child, child_text.len(), child_text.len())
        })
        .collect();
    
    // Calculate cumulative sizes
    let mut cumulative = 0usize;
    let mut children_with_cumulative: Vec<(Node, usize, usize)> = Vec::new();
    for (child, size, _) in child_sizes {
        cumulative += size;
        children_with_cumulative.push((child, size, cumulative));
    }
    
    let total_content_size = cumulative;
    
    // Check if this node itself is too big
    if total_size > config.target_size && partitionable_children.len() == 1 {
        // Single child that makes us too big - try to split the child
        partition_node(partitionable_children[0], source, config, breadcrumb_prefix, &new_class_context, depth + 1, file_path, catalog, content_hash, chunks);
        return;
    }
    
    // If total content is small enough per-child, just partition all children
    if total_content_size <= config.target_size * partitionable_children.len() {
        for child in partitionable_children {
            partition_node(child, source, config, breadcrumb_prefix, &new_class_context, depth + 1, file_path, catalog, content_hash, chunks);
        }
        return;
    }
    
    // Need to split into groups based on cumulative text size
    let mut current_group_start = 0;
    let mut current_group_size = 0;
    
    for (i, (_child, size, _)) in children_with_cumulative.iter().enumerate() {
        current_group_size += size;
        
        if current_group_size >= config.target_size {
            for j in current_group_start..=i {
                partition_node(
                    partitionable_children[j],
                    source,
                    config,
                    breadcrumb_prefix,
                    &new_class_context,
                    depth + 1,
                    file_path,
                    catalog,
                    content_hash,
                    chunks,
                );
            }
            
            current_group_start = i + 1;
            current_group_size = 0;
        }
    }
    
    // Handle remaining children
    for j in current_group_start..partitionable_children.len() {
        partition_node(
            partitionable_children[j],
            source,
            config,
            breadcrumb_prefix,
            &new_class_context,
            depth + 1,
            file_path,
            catalog,
            content_hash,
            chunks,
        );
    }
}

/// Split an oversized leaf node using AST-based split points
/// Falls back to line-based splitting if AST splitting fails
fn split_oversized_leaf(
    node: Node,
    source: &[u8],
    config: &PartitionConfig,
    breadcrumb: &str,
    start_line_base: usize,
    file_path: &str,
    catalog: &str,
    content_hash: &str,
    chunks: &mut Vec<PartitionedChunk>,
) {
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    let text = String::from_utf8_lossy(&source[start_byte..end_byte]).to_string();
    let chunk_type = get_chunk_type(node);
    
    // Find AST-based split points
    let split_points = find_ast_split_points(node, source, config.target_size);

    
    if split_points.is_empty() {
        // No natural split points found - fall back to line-based splitting
        split_by_lines(&text, start_line_base, config, breadcrumb, chunk_type, file_path, catalog, content_hash, chunks);
        return;
    }
    
    // Emit chunks at AST split points
    let lines_vec: Vec<&str> = text.lines().collect();
    
    for (i, (start_idx, end_idx)) in split_points.iter().enumerate() {
        let part_text = lines_vec[*start_idx..*end_idx].join("\n");
        let part_size = part_text.len();
        let start_line = start_line_base + start_idx;
        let end_line = start_line_base + end_idx.saturating_sub(1);
        
        // If this chunk is still oversized, try line-based splitting
        if part_size > config.target_size {
            split_by_lines(&part_text, start_line, config, breadcrumb, chunk_type.clone(), file_path, catalog, content_hash, chunks);
        } else {
            chunks.push(PartitionedChunk {
                file: file_path.to_string(),
                catalog: catalog.to_string(),
                content_hash: content_hash.to_string(),
                breadcrumb: format!("{} (part {}/{})", breadcrumb, i + 1, split_points.len()),
                text: part_text,
                start_line,
                end_line,
                chunk_type: chunk_type.clone(),
                symbol_name: None,
            });
        }
    }
}

/// Find AST-based split points for an oversized node
/// Returns a list of (start_line, end_line) tuples for each chunk
fn find_ast_split_points(node: Node, source: &[u8], target_size: usize) -> Vec<(usize, usize)> {
    let mut split_candidates = Vec::new();
    
    // Get the starting line of this node (to convert absolute to relative)
    let node_start_line = node.start_position().row;
    
    // Traverse to find split-able statements
    find_split_candidates(node, source, node_start_line, &mut split_candidates);
    
    if split_candidates.is_empty() {
        return Vec::new();
    }
    
    // Convert absolute line numbers to relative (0-based within this node)
    let relative_candidates: Vec<usize> = split_candidates
        .iter()
        .map(|&abs_line| abs_line.saturating_sub(node_start_line))
        .collect();
    
    // Get the text of this node
    let text = String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]);
    let lines_vec: Vec<&str> = text.lines().collect();
    
    // Build chunks by walking through lines and splitting at candidates when size exceeds target
    let mut split_points = Vec::new();
    let mut chunk_start = 0;
    
    // Add 0 as implicit first candidate, and lines_vec.len() as last
    let mut all_candidates = vec![0];
    all_candidates.extend(relative_candidates.iter().cloned());
    all_candidates.push(lines_vec.len());
    all_candidates.sort();
    all_candidates.dedup();
    
    for i in 1..all_candidates.len() {
        let prev_candidate = all_candidates[i - 1];
        let curr_candidate = all_candidates[i];
        
        if curr_candidate > lines_vec.len() || prev_candidate >= curr_candidate {
            continue;
        }
        
        // Calculate size from chunk_start to curr_candidate
        let segment_size = lines_vec[chunk_start..curr_candidate].join("\n").len();
        
        if segment_size > target_size && chunk_start < prev_candidate {
            // Split at previous candidate
            split_points.push((chunk_start, prev_candidate));
            chunk_start = prev_candidate;
        }
    }
    
    // Add final chunk
    if chunk_start < lines_vec.len() {
        split_points.push((chunk_start, lines_vec.len()));
    }
    
    // Validate and deduplicate
    split_points.retain(|(start, end)| start < end && *end <= lines_vec.len());
    split_points.sort_by_key(|(start, _)| *start);
    split_points.dedup();
    
        // eprintln!("DEBUG: Found {} split points: {:?}", split_points.len(), split_points);
    split_points
}

/// Find candidate split points by looking for major AST structures
fn find_split_candidates(node: Node, source: &[u8], line_offset: usize, candidates: &mut Vec<usize>) {
    let start_line = node.start_position().row;
    let absolute_line = line_offset + start_line;
    
    // Check if this node is a good split point
    if is_good_split_point(node, source) {
        // eprintln!("DEBUG: Found good split point at line {}: {}", absolute_line, node.kind());
        candidates.push(absolute_line);
    }
    
    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_split_candidates(child, source, line_offset, candidates);
    }
}

/// Check if this node is a good split point
fn is_good_split_point(node: Node, _source: &[u8]) -> bool {
    match node.kind() {
        // Statement-level structures are good split points
        "statement_block" => {
            // Check if this is a top-level block (not nested too deep)
            let parent = node.parent();
            if let Some(p) = parent {
                !matches!(p.kind(), "statement_block" | "if_statement" | "for_statement")
            } else {
                false
            }
        }
        
        // Control flow statements
        "if_statement" | "try_statement" | "switch_statement" | "for_statement" 
        | "for_in_statement" | "while_statement" | "do_statement" => true,
        
        // Return statements (good phase boundaries)
        "return_statement" => true,
        
        // Enum and object members (for large enums/objects)
        "enum_assignment" | "enum_declaration" => true,
        
        // Comments with substantial content (JSDoc blocks)
        "comment" => true,
        
        // Variable declarations (phase boundaries)
        "variable_declaration" | "lexical_declaration" => true,
        
        // Expression statements can be split points
        "expression_statement" => {
            // Only if it's a substantial expression
            let size = node.end_byte() - node.start_byte();
            size > 100
        }
        
        _ => false,
    }
}

/// Fallback: split by lines when AST splitting fails
fn split_by_lines(
    text: &str,
    start_line_base: usize,
    config: &PartitionConfig,
    breadcrumb: &str,
    chunk_type: String,
    file_path: &str,
    catalog: &str,
    content_hash: &str,
    chunks: &mut Vec<PartitionedChunk>,
) {
    let lines_vec: Vec<&str> = text.lines().collect();
    let mut split_points: Vec<(usize, usize)> = Vec::new();
    let mut current_size = 0;
    let mut current_start = 0;
    
    for (i, line) in lines_vec.iter().enumerate() {
        let line_size = line.len() + 1;
        if current_size + line_size > config.target_size && current_size > 0 {
            split_points.push((current_start, i));
            current_size = 0;
            current_start = i;
        }
        current_size += line_size;
    }
    
    if current_start < lines_vec.len() {
        split_points.push((current_start, lines_vec.len()));
    }
    
    for (part_num, (start_idx, end_idx)) in split_points.iter().enumerate() {
        let part_text = lines_vec[*start_idx..*end_idx].join("\n");
        let start_line = start_line_base + start_idx;
        let end_line = start_line_base + end_idx.saturating_sub(1);
        
        chunks.push(PartitionedChunk {
            file: file_path.to_string(),
            catalog: catalog.to_string(),
            content_hash: content_hash.to_string(),
            breadcrumb: format!("{} (part {}/{})", breadcrumb, part_num + 1, split_points.len()),
            text: part_text,
            start_line,
            end_line,
            chunk_type: chunk_type.clone(),
            symbol_name: None,
        });
    }
}

/// Get children that can be partitioned (methods, functions, classes, etc.)
fn get_partitionable_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    let mut result = Vec::new();
    
    for child in node.children(&mut cursor) {
        if is_meaningful_node(child) {
            result.push(child);
        }
    }
    
    result
}

/// Check if a node is meaningful for partitioning
fn is_meaningful_node(node: Node) -> bool {
    match node.kind() {
        "function_declaration" |
        "method_definition" |
        "class_declaration" |
        "class" |
        "interface_declaration" |
        "type_alias_declaration" |
        "enum_declaration" |
        "export_statement" |
        "variable_declaration" |
        "lexical_declaration" => true,
        
        "comment" | "whitespace" => false,
        
        _ => {
            let size = node.end_byte() - node.start_byte();
            size > 50
        }
    }
}

/// Extract text including preceding JSDoc comment
fn extract_text_with_preceding_comments(node: Node, source: &[u8]) -> (String, usize) {
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    let _start_line = node.start_position().row + 1;
    
    // Check for preceding JSDoc comment
    let actual_start = if let Some(comment) = find_preceding_comment(node, source) {
        comment.start_byte()
    } else {
        start_byte
    };
    
    let actual_start_line = source[0..actual_start]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() + 1;
    
    let text = String::from_utf8_lossy(&source[actual_start..end_byte]).to_string();
    (text, actual_start_line)
}

/// Find the JSDoc comment immediately preceding a node
fn find_preceding_comment<'a>(node: Node<'a>, source: &'a [u8]) -> Option<Node<'a>> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let mut prev_sibling: Option<Node> = None;
    
    for child in parent.children(&mut cursor) {
        if child == node {
            break;
        }
        prev_sibling = Some(child);
    }
    
    if let Some(prev) = prev_sibling {
        if prev.kind() == "comment" {
            let comment_text = String::from_utf8_lossy(&source[prev.start_byte()..prev.end_byte()]);
            if comment_text.trim_start().starts_with("/**") {
                return Some(prev);
            }
        }
    }
    
    None
}

/// Build breadcrumb for a node: package:file:class:symbol
fn build_breadcrumb(node: Node, source: &[u8], prefix: &str, class_context: &str, depth: usize) -> String {
    if depth == 0 {
        return prefix.to_string();
    }
    
    if let Some(name) = get_symbol_name(node, source) {
        if !class_context.is_empty() && depth == 1 {
            // At class level - just add class to prefix
            format!("{}:{}", prefix, name)
        } else if !class_context.is_empty() && depth > 1 {
            // Inside class - add class and method
            format!("{}:{}:{}", prefix, class_context, name)
        } else {
            // Top-level symbol
            format!("{}:{}", prefix, name)
        }
    } else {
        prefix.to_string()
    }
}

/// Get the chunk type for a node
fn get_chunk_type(node: Node) -> String {
    match node.kind() {
        "function_declaration" | "method_definition" => "function".to_string(),
        "arrow_function" => "arrow_function".to_string(),
        "class_declaration" | "class" => "class".to_string(),
        "interface_declaration" => "interface".to_string(),
        "type_alias_declaration" => "type".to_string(),
        "enum_declaration" => "enum".to_string(),
        "property_declaration" | "public_field_definition" => "field".to_string(),
        _ => "code".to_string(),
    }
}

/// Get the symbol name from a node
fn get_symbol_name(node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "property_identifier" | "type_identifier" => {
                let name = String::from_utf8_lossy(&source[child.start_byte()..child.end_byte()]);
                return Some(name.to_string());
            }
            _ => {}
        }
    }
    
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;

    fn format_chunks(chunks: &[PartitionedChunk]) -> String {
        let mut result = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            result.push_str(&format!(
                "=== CHUNK {} ===\nBreadcrumb: {}\nType: {}\nSymbol: {:?}\nLines: {}-{}\nSize: {} chars\nText preview (5 lines):\n{}\n\n",
                i + 1,
                chunk.breadcrumb,
                chunk.chunk_type,
                chunk.symbol_name,
                chunk.start_line,
                chunk.end_line,
                chunk.breadcrumb.len() + chunk.text.len(),
                chunk.text.lines().take(5).collect::<Vec<_>>().join("\n")
            ));
        }
        result
    }
    
    #[test]
    fn test_simple_function() {
        let source = r#"
export function add(a: number, b: number): number {
    return a + b;
}
"#;
        let config = PartitionConfig {
            file_name: "test.ts".to_string(),
            package_name: "@test/package".to_string(),
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "test.ts", "test");
        assert_snapshot!(chunks.len());
        assert_snapshot!(format_chunks(&chunks));
    }
    
    #[test]
    fn test_class_with_methods() {
        let source = r#"
/**
 * A simple calculator class.
 */
export class Calculator {
    /**
     * Adds two numbers.
     */
    add(a: number, b: number): number {
        return a + b;
    }
    
    /**
     * Subtracts two numbers.
     */
    subtract(a: number, b: number): number {
        return a - b;
    }
    
    /**
     * Multiplies two numbers.
     */
    multiply(a: number, b: number): number {
        return a * b;
    }
}
"#;
        let config = PartitionConfig {
            file_name: "Calculator.ts".to_string(),
            package_name: "@math/package".to_string(),
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "Calculator.ts", "math");
        assert_snapshot!(format_chunks(&chunks));
    }
    
    #[test]
    fn test_jsonfile_partition() {
        let source = include_str!("../../test_artifacts/JsonFile.ts");
        let config = PartitionConfig {
            file_name: "JsonFile.ts".to_string(),
            package_name: "@rushstack/node-core-library".to_string(),
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "JsonFile.ts", "node-core-library");
        
        println!("\n=== PARTITION RESULTS ===");
        println!("Total chunks: {}", chunks.len());
        
        let mut oversized = 0;
        for chunk in &chunks {
            let text_size = chunk.text.len();
            let total_size = chunk.breadcrumb.len() + chunk.text.len();
            if text_size > 1800 {
                oversized += 1;
                println!("⚠️  {}: {} chars (text: {})", chunk.breadcrumb, total_size, text_size);
            } else {
                println!("✓ {}: {} chars", chunk.breadcrumb, total_size);
            }
        }
        
        println!("\nOversized text chunks (>1800): {}", oversized);
        
        assert!(chunks.len() > 10, "Should have multiple chunks");
        // assert!(oversized < 2, "Should have minimal oversized chunks");
        
        assert_snapshot!(format_chunks(&chunks));
    }

    
    #[test]
    fn test_token_count_check() {
        use tokenizers::Tokenizer;
        
        // Load a tokenizer similar to our embedding model
        let tokenizer = Tokenizer::from_file("models/tokenizer.json").ok();
        
        let source = include_str!("../../test_artifacts/JsonFile.ts");
        let config = PartitionConfig {
            file_name: "JsonFile.ts".to_string(),
            package_name: "@rushstack/node-core-library".to_string(),
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "JsonFile.ts", "node-core-library");
        
        println!("\n=== TOKEN COUNT CHECK ===");
        println!("Checking chunks that exceed 512 tokens...");
        
        if let Some(tokenizer) = tokenizer {
            for chunk in &chunks {
                let encoding = tokenizer.encode(chunk.text.as_str(), true).unwrap();
                let token_count = encoding.len();
                
                if token_count > 512 {
                    println!("⚠️  {}: {} chars, {} tokens (EXCEEDS 512!)", 
                        chunk.breadcrumb, chunk.text.len(), token_count);
                }
            }
        } else {
            println!("Tokenizer not available - skipping token count check");
            println!("Run 'cargo build' first to download the model");
        }
    }

}
