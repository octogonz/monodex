//! Partition-based code chunking
//!
//! ## Algorithm: Two Worlds Model
//!
//! The algorithm coordinates two separate concerns:
//!
//! **Chunk Land (sizing/selection):**
//! - The file is a sequence of line ranges (chunks)
//! - Can measure any chunk's size in characters
//! - Can split a chunk at a given line number
//! - Knows the budget and when we're done
//!
//! **AST Land (structure/meaning):**
//! - Recursively walks the syntax tree
//! - Provides candidate split points as line numbers
//! - "Meaningful" = doesn't break semantic units
//!
//! **Coordination:**
//! 1. Start with one chunk = entire file
//! 2. While any chunk exceeds budget:
//!    a. AST land provides meaningful split points within that chunk
//!    b. Chunk land picks the best split and divides the chunk
//! 3. Done - all chunks fit budget

use tree_sitter::{Node, Parser};
use super::util::compute_hash;

/// Target chunk size in lines (derived from 6000 char target, ~50 chars/line)
const TARGET_LINES: usize = 120;

/// Compute quality score for chunking results (0-100%, higher is better).
///
/// The score combines two factors:
/// 1. Count badness: How many more chunks than ideal (0 if at ideal, 1 if all 1-line chunks)
/// 2. Micro-chunk badness: How small the chunks are relative to ideal (0 if all at max size)
///
/// Final score = 100 × (1 - count_badness) × (1 - micro_badness)³
pub fn chunk_quality_score(chunks: &[PartitionedChunk], file_lines: usize) -> f64 {
    if chunks.is_empty() || file_lines == 0 {
        return 100.0;
    }
    
    let max_chunk_size = TARGET_LINES.min(file_lines);
    let chunk_count = chunks.len();
    
    // Compute chunk sizes
    let chunk_sizes: Vec<usize> = chunks
        .iter()
        .map(|c| c.end_line - c.start_line + 1)
        .collect();
    
    let total_lines: usize = chunk_sizes.iter().sum();
    
    // Ideal number of chunks
    let ideal_chunk_count = (total_lines + max_chunk_size - 1) / max_chunk_size; // ceil division
    
    // 1) Count badness: 0 at ideal chunk count, 1 at all 1-line chunks
    let count_badness = if total_lines == ideal_chunk_count {
        0.0
    } else {
        (chunk_count as f64 - ideal_chunk_count as f64) / (total_lines as f64 - ideal_chunk_count as f64)
    };
    
    // Helper: chunk badness (0 at max size or larger, 1 at 1 line)
    let chunk_badness = |size: usize| -> f64 {
        if size >= max_chunk_size {
            0.0
        } else {
            ((max_chunk_size - size) as f64 / (max_chunk_size - 1) as f64).powi(2)
        }
    };
    
    // 2) Micro-chunk badness relative to ideal partition
    let ideal_last_chunk_size = total_lines - max_chunk_size * (ideal_chunk_count.saturating_sub(1));
    let ideal_partition_badness = if ideal_chunk_count == 0 {
        0.0
    } else if ideal_chunk_count == 1 {
        chunk_badness(ideal_last_chunk_size)
    } else {
        // All but last chunk are at max size (badness 0), last chunk may be smaller
        chunk_badness(ideal_last_chunk_size)
    };
    
    let actual_partition_badness: f64 = chunk_sizes.iter().map(|&s| chunk_badness(s)).sum();
    let worst_partition_badness = total_lines as f64; // all 1-line chunks, each with badness 1
    
    let micro_badness = if worst_partition_badness == ideal_partition_badness {
        0.0
    } else {
        (actual_partition_badness - ideal_partition_badness) / (worst_partition_badness - ideal_partition_badness)
    };
    
    // Clamp for numerical safety
    let count_badness = count_badness.clamp(0.0, 1.0);
    let micro_badness = micro_badness.clamp(0.0, 1.0);
    
    // Final score: weight micro_badness more heavily (beta=3)
    let alpha = 1.0;
    let beta = 3.0;
    let score = 100.0 * (1.0 - count_badness).powf(alpha) * (1.0 - micro_badness).powf(beta);
    
    score.clamp(0.0, 100.0)
}

/// Quality report for chunking results
pub struct ChunkQualityReport {
    /// Quality score (0-100%, higher is better)
    pub score: f64,
    /// Total number of chunks
    pub total_chunks: usize,
    /// Number of chunks under 20 lines (likely problematic)
    pub tiny_chunks: usize,
    /// Smallest chunk in lines
    pub min_lines: usize,
    /// Largest chunk in lines
    pub max_lines: usize,
    /// Mean chunk size in lines
    pub mean_lines: f64,
}

impl ChunkQualityReport {
    pub fn from_chunks(chunks: &[PartitionedChunk], file_lines: usize) -> Self {
        if chunks.is_empty() {
            return Self {
                score: 100.0,
                total_chunks: 0,
                tiny_chunks: 0,
                min_lines: 0,
                max_lines: 0,
                mean_lines: 0.0,
            };
        }
        
        let line_counts: Vec<usize> = chunks
            .iter()
            .map(|c| c.end_line - c.start_line + 1)
            .collect();
        
        Self {
            score: chunk_quality_score(chunks, file_lines),
            total_chunks: chunks.len(),
            tiny_chunks: line_counts.iter().filter(|&&l| l < 20).count(),
            min_lines: *line_counts.iter().min().unwrap(),
            max_lines: *line_counts.iter().max().unwrap(),
            mean_lines: line_counts.iter().sum::<usize>() as f64 / line_counts.len() as f64,
        }
    }
    
    pub fn format(&self) -> String {
        format!(
            "Score: {:.1}% | Chunks: {} | Tiny (<20 lines): {} | Lines: {}-{} (mean {:.1})",
            self.score,
            self.total_chunks,
            self.tiny_chunks,
            self.min_lines,
            self.max_lines,
            self.mean_lines
        )
    }
}

/// Configuration for partition chunking
pub struct PartitionConfig {
    /// Target chunk size in characters (text only, breadcrumb is extra)
    pub target_size: usize,
    
    /// File name for breadcrumb prefix
    pub file_name: String,
    
    /// Package name for breadcrumb (e.g., "@rushstack/node-core-library")
    pub package_name: String,
}

impl Default for PartitionConfig {
    fn default() -> Self {
        Self {
            target_size: 6000,
            file_name: "unknown.ts".to_string(),
            package_name: "unknown".to_string(),
        }
    }
}

/// A chunk of code with breadcrumb context
#[derive(Debug, Clone)]
pub struct PartitionedChunk {
    /// Source URI (file path, issue reference, etc.)
    pub source_uri: String,
    
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
    
    /// Chunk kind (content, imports, changelog, config)
    pub chunk_kind: String,
    
    /// Symbol name (if applicable)
    pub symbol_name: Option<String>,
}

/// A line range representing a chunk-in-progress
#[derive(Debug, Clone)]
struct ChunkRange {
    start_line: usize,  // 1-indexed, inclusive
    end_line: usize,    // 1-indexed, inclusive
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
    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len();
    
    // Build base breadcrumb: package:file
    let base_breadcrumb = if config.package_name.is_empty() {
        config.file_name.clone()
    } else {
        format!("{}:{}", config.package_name, config.file_name)
    };
    
    // Step 1: Start with the whole file as one chunk
    let mut chunks: Vec<ChunkRange> = vec![ChunkRange { start_line: 1, end_line: total_lines }];
    
    // Also extract imports end line for chunk_kind metadata (but don't pre-split)
    let import_end_line = extract_imports_end_line(root, source.as_bytes());
    
    // Step 2: Iteratively split chunks that exceed budget
    let mut changed = true;
    while changed {
        changed = false;
        let mut new_chunks = Vec::new();
        
        for chunk_range in &chunks {
            let chunk_text = get_lines_text(&lines, chunk_range.start_line, chunk_range.end_line);
            let chunk_size = chunk_text.len();
            
            if chunk_size <= config.target_size {
                new_chunks.push(chunk_range.clone());
            } else {
                // Chunk is too big - find the deepest node spanning this range
                let spanning = find_spanning_node(root, chunk_range.start_line, chunk_range.end_line);
                
                // Get meaningful children of the spanning node
                let mut children = get_meaningful_children(spanning, source.as_bytes());
                
                // Filter to children that are within the chunk range
                children.retain(|child| {
                    let child_start = child.start_position().row + 1;
                    let child_end = child.end_position().row + 1;
                    child_end >= chunk_range.start_line && child_start <= chunk_range.end_line
                });
                
                // Keep diving until we find 2+ meaningful children or can't go deeper
                // This handles: export_statement -> class_declaration -> class_body -> methods
                let mut current_node = if children.len() == 1 { Some(children[0]) } else { None };
                
                while let Some(node) = current_node {
                    let mut inner_children = get_meaningful_children(node, source.as_bytes());
                    
                    // If no meaningful children, look inside structural containers
                    if inner_children.is_empty() {
                        inner_children = find_children_in_container(node);
                    }
                    
                    if inner_children.len() >= 2 {
                        // Found 2+ children - use them as split candidates
                        children = inner_children;
                        break;
                    } else if inner_children.len() == 1 {
                        // Only 1 child - keep diving
                        current_node = Some(inner_children[0]);
                    } else {
                        // No children - can't go deeper
                        break;
                    }
                }
                
                if children.len() < 2 {
                    // Can't split - no siblings to split between
                    new_chunks.push(chunk_range.clone());
                } else {
                    // Find split points between these siblings
                    let split_points: Vec<usize> = children
                        .iter()
                        .take(children.len() - 1)
                        .map(|child| child.end_position().row + 1)
                        .filter(|&line| line >= chunk_range.start_line && line < chunk_range.end_line)
                        .collect();
                    
                    let best_split = pick_best_split(&split_points, chunk_range.start_line, chunk_range.end_line, chunk_size, config.target_size);
                    
                    if let Some(split_line) = best_split {
                        if split_line >= chunk_range.start_line && split_line < chunk_range.end_line {
                            new_chunks.push(ChunkRange { start_line: chunk_range.start_line, end_line: split_line });
                            new_chunks.push(ChunkRange { start_line: split_line + 1, end_line: chunk_range.end_line });
                            changed = true;
                        } else {
                            new_chunks.push(chunk_range.clone());
                        }
                    } else {
                        new_chunks.push(chunk_range.clone());
                    }
                }
            }
        }
        chunks = new_chunks;
    }
    
    // Step 3: Convert chunk ranges to PartitionedChunks
    let mut result = Vec::new();
    
    for chunk_range in &chunks {
        let chunk_text = get_lines_text(&lines, chunk_range.start_line, chunk_range.end_line);
        if chunk_text.trim().is_empty() { continue; }
        
        let (chunk_type, symbol_name, breadcrumb_suffix) = get_chunk_metadata(
            root, source.as_bytes(), chunk_range.start_line, chunk_range.end_line,
        );
        
        let breadcrumb = if breadcrumb_suffix.is_empty() {
            base_breadcrumb.clone()
        } else {
            format!("{}:{}", base_breadcrumb, breadcrumb_suffix)
        };
        
        let chunk_kind = if import_end_line > 0 && chunk_range.end_line <= import_end_line {
            "imports".to_string()
        } else {
            "content".to_string()
        };
        
        result.push(PartitionedChunk {
            source_uri: file_path.to_string(),
            catalog: catalog.to_string(),
            content_hash: content_hash.to_string(),
            breadcrumb,
            text: chunk_text,
            start_line: chunk_range.start_line,
            end_line: chunk_range.end_line,
            chunk_type,
            chunk_kind,
            symbol_name,
        });
    }
    
    result
}

/// Pick the best split point - prefer splits that create balanced chunks
fn pick_best_split(
    split_points: &[usize],
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    target_size: usize,
) -> Option<usize> {
    if split_points.is_empty() {
        return None;
    }
    
    // Find split that creates two chunks as close to target as possible
    for &split_line in split_points {
        let lines_before = split_line - start_line + 1;
        let total_lines = end_line - start_line + 1;
        let estimated_first_size = (chunk_size * lines_before) / total_lines;
        
        if estimated_first_size >= target_size / 4 && estimated_first_size <= target_size {
            return Some(split_line);
        }
    }
    
    split_points.first().copied()
}

/// Find the deepest AST node that spans the entire line range
fn find_spanning_node<'a>(node: Node<'a>, start_line: usize, end_line: usize) -> Node<'a> {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;
    
    if node_start > start_line || node_end < end_line {
        return node;
    }
    
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;
        
        if child_start <= start_line && child_end >= end_line {
            return find_spanning_node(child, start_line, end_line);
        }
    }
    
    node
}

/// Get meaningful children of a node (methods, classes, functions, etc.)
fn get_meaningful_children<'a>(node: Node<'a>, source: &[u8]) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    let mut children = Vec::new();
    
    for child in node.children(&mut cursor) {
        if is_meaningful_split_point(child, source) {
            children.push(child);
        }
    }
    
    children
}

/// Find children inside structural containers (class_body, export_statement, etc.)
/// Returns direct children of the container that are meaningful split points.
fn find_children_in_container<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    
    // Check if this node IS a container we want to look inside
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "class_body" | "declaration_list" | "statement_block" | "object_type") {
            // Found a container - return its meaningful children
            let mut container_cursor = child.walk();
            let mut result = Vec::new();
            for grandchild in child.children(&mut container_cursor) {
                if matches!(grandchild.kind(), 
                    "method_definition" | "public_field_definition" | 
                    "function_declaration" | "class_declaration" | 
                    "interface_declaration" | "type_alias_declaration" | 
                    "enum_declaration" | "lexical_declaration") {
                    result.push(grandchild);
                }
            }
            return result;
        }
    }
    
    Vec::new()
}

/// Get the end line of import statements (0 if no imports)
fn extract_imports_end_line(root: Node, source: &[u8]) -> usize {
    let mut cursor = root.walk();
    let mut last_import_end = 0usize;
    
    for child in root.children(&mut cursor) {
        if child.kind() == "import_statement" {
            last_import_end = child.end_position().row + 1;
        } else if last_import_end > 0 {
            break;
        }
    }
    last_import_end
}

/// Get text for a line range (1-indexed, inclusive)
fn get_lines_text(lines: &[&str], start_line: usize, end_line: usize) -> String {
    if start_line > end_line || start_line == 0 || start_line > lines.len() {
        return String::new();
    }
    let end = end_line.min(lines.len());
    lines[start_line - 1..end].join("\n")
}

/// Find meaningful split points within a line range
fn find_meaningful_split_points(
    root: Node,
    source: &[u8],
    start_line: usize,
    end_line: usize,
) -> Vec<usize> {
    let mut candidates: Vec<usize> = Vec::new();
    collect_split_candidates(root, source, start_line, end_line, &mut candidates);
    candidates.sort();
    candidates.dedup();
    candidates.into_iter().filter(|&line| line >= start_line && line < end_line).collect()
}

fn collect_split_candidates(
    node: Node,
    source: &[u8],
    range_start: usize,
    range_end: usize,
    candidates: &mut Vec<usize>,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;
    
    if node_end < range_start || node_start > range_end { return; }
    
    if is_meaningful_split_point(node, source) {
        // Add split point AFTER this node ends
        // This is "between siblings" - after one sibling, before the next
        if node_end >= range_start && node_end < range_end {
            candidates.push(node_end);
        }
    }
    
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_split_candidates(child, source, range_start, range_end, candidates);
    }
}

/// Find the start line of a node, including any preceding JSDoc comment
fn find_node_start_with_comment(node: Node, source: &[u8]) -> usize {
    let node_start = node.start_position().row + 1;
    
    // Look for preceding JSDoc comment
    if let Some(comment) = find_preceding_jsdoc(node, source) {
        comment.start_position().row + 1
    } else {
        node_start
    }
}

/// Find the JSDoc comment immediately preceding a node
fn find_preceding_jsdoc<'a>(node: Node<'a>, source: &[u8]) -> Option<Node<'a>> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let mut siblings: Vec<Node> = Vec::new();
    
    for child in parent.children(&mut cursor) {
        if child == node {
            break;
        }
        siblings.push(child);
    }
    
    // Walk backwards through siblings looking for a JSDoc comment
    for sibling in siblings.into_iter().rev() {
        if sibling.kind() == "comment" {
            let comment_text = String::from_utf8_lossy(&source[sibling.start_byte()..sibling.end_byte()]);
            if comment_text.trim_start().starts_with("/**") {
                return Some(sibling);
            }
            // Non-JSDoc comment - keep looking
        } else if sibling.kind() != "comment" {
            // Non-comment node - stop looking
            break;
        }
    }
    
    None
}

fn is_meaningful_split_point(node: Node, source: &[u8]) -> bool {
    match node.kind() {
        "function_declaration" | "class_declaration" | "interface_declaration" |
        "type_alias_declaration" | "enum_declaration" => true,
        
        "export_statement" => {
            let mut cursor = node.walk();
            node.children(&mut cursor).any(|c| matches!(c.kind(),
                "function_declaration" | "class_declaration" | "interface_declaration" |
                "type_alias_declaration" | "enum_declaration"))
        }
        
        "method_definition" => true,
        
        "public_field_definition" | "property_declaration" => {
            node.end_byte() - node.start_byte() > 50
        }
        
        "lexical_declaration" | "variable_declaration" => {
            let text = String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]);
            text.starts_with("const") || text.starts_with("let") || text.starts_with("var")
        }
        
        _ => false,
    }
}

/// Get chunk metadata (type, symbol name, breadcrumb suffix) for a line range
fn get_chunk_metadata(
    root: Node,
    source: &[u8],
    start_line: usize,
    end_line: usize,
) -> (String, Option<String>, String) {
    let mut best_node: Option<Node> = None;
    let mut best_size = usize::MAX;
    
    find_best_node(root, source, start_line, end_line, &mut best_node, &mut best_size);
    
    if let Some(node) = best_node {
        let chunk_type = get_chunk_type(node);
        let symbol_name = get_symbol_name(node, source);
        let breadcrumb_suffix = symbol_name.clone().unwrap_or_default();
        (chunk_type, symbol_name, breadcrumb_suffix)
    } else {
        ("code".to_string(), None, String::new())
    }
}

fn find_best_node<'a>(
    node: Node<'a>,
    source: &[u8],
    chunk_start: usize,
    chunk_end: usize,
    best_node: &mut Option<Node<'a>>,
    best_size: &mut usize,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;
    
    if node_start > chunk_start || node_end < chunk_end {
        return;
    }
    
    if is_meaningful_split_point(node, source) {
        let node_size = node_end - node_start;
        if node_size < *best_size {
            *best_node = Some(node);
            *best_size = node_size;
        }
    }
    
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_best_node(child, source, chunk_start, chunk_end, best_node, best_size);
    }
}

fn get_chunk_type(node: Node) -> String {
    if node.kind() == "export_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_type = match child.kind() {
                "function_declaration" => "function",
                "class_declaration" => "class",
                "interface_declaration" => "interface",
                "type_alias_declaration" => "type",
                "enum_declaration" => "enum",
                "lexical_declaration" | "variable_declaration" => "variable",
                _ => continue,
            };
            return child_type.to_string();
        }
        return "code".to_string();
    }
    
    match node.kind() {
        "function_declaration" | "method_definition" => "function",
        "class_declaration" => "class",
        "interface_declaration" => "interface",
        "type_alias_declaration" => "type",
        "enum_declaration" => "enum",
        "lexical_declaration" | "variable_declaration" => "variable",
        "public_field_definition" | "property_declaration" => "field",
        _ => "code",
    }.to_string()
}

fn get_symbol_name(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() == "export_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(name) = get_symbol_name(child, source) {
                return Some(name);
            }
        }
        return None;
    }
    
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

    fn format_chunks_summary(chunks: &[PartitionedChunk], file_lines: usize) -> String {
        let report = ChunkQualityReport::from_chunks(chunks, file_lines);
        let mut result = format!(
            "=== QUALITY SCORE ===\nScore: {:.1}%\nTotal chunks: {}\nTiny chunks (<20 lines): {}\nLines: {}-{} (mean {:.1})\n\n",
            report.score,
            report.total_chunks,
            report.tiny_chunks,
            report.min_lines,
            report.max_lines,
            report.mean_lines
        );
        for (i, chunk) in chunks.iter().enumerate() {
            result.push_str(&format!(
                "=== CHUNK {} ===\nBreadcrumb: {}\nType: {}\nKind: {}\nSymbol: {:?}\nLines: {}-{}\nSize: {} chars\nText preview (5 lines):\n{}\n\n",
                i + 1,
                chunk.breadcrumb,
                chunk.chunk_type,
                chunk.chunk_kind,
                chunk.symbol_name,
                chunk.start_line,
                chunk.end_line,
                chunk.breadcrumb.len() + chunk.text.len(),
                chunk.text.lines().take(5).collect::<Vec<_>>().join("\n")
            ));
        }
        result
    }
    
    /// Visualize chunks as split points in the original source
    fn format_chunks_visualization(source: &str, chunks: &[PartitionedChunk]) -> String {
        let lines: Vec<&str> = source.lines().collect();
        let mut result = String::new();
        
        for (i, chunk) in chunks.iter().enumerate() {
            let line_count = chunk.end_line - chunk.start_line + 1;
            let size = chunk.text.len();
            
            result.push_str(&format!(
                "-- [CHUNK {}] [{} lines] [{} chars] --\n",
                i + 1, line_count, size
            ));
            
            for line_num in chunk.start_line..=chunk.end_line {
                if line_num > 0 && line_num <= lines.len() {
                    result.push_str(lines[line_num - 1]);
                    result.push('\n');
                }
            }
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
        assert_snapshot!(format_chunks_summary(&chunks, source.lines().count()));
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
        assert_snapshot!(format_chunks_summary(&chunks, source.lines().count()));
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
        
        // Snapshot 1: Visualization of split points in the source
        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("jsonfile_visualization", visualization);
        
        // Snapshot 2: Metadata summary
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("jsonfile_summary", summary);
    }
    
    #[test]
    fn test_small_file_not_penalized() {
        // A 10-line file with one chunk should score 100%
        // (not penalized for being "tiny" since the whole file is tiny)
        let source = r#"// Small test file
export function tiny(): number {
    return 42;
}
"#;
        let config = PartitionConfig {
            file_name: "tiny.ts".to_string(),
            package_name: "@test/package".to_string(),
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "tiny.ts", "test");
        let file_lines = source.lines().count();
        let score = chunk_quality_score(&chunks, file_lines);
        
        // Small file with one chunk should score 100%
        assert_eq!(chunks.len(), 1);
        assert!(score > 99.0, "Small file with one chunk should score ~100%, got {:.1}%", score);
    }
    
    #[test]
    fn test_small_file_should_not_split() {
        // A 12-line .d.ts file (242 chars) should NOT be split into 2 chunks
        // This is a regression test for the "imports always split" bug
        let source = include_str!("../../test_artifacts/rollup.d.ts");
        let config = PartitionConfig {
            file_name: "rollup.d.ts".to_string(),
            package_name: "api-extractor-scenarios".to_string(),
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "rollup.d.ts", "api-extractor-scenarios");
        let file_lines = source.lines().count();
        let score = chunk_quality_score(&chunks, file_lines);
        
        // The whole file is only 242 chars - should be one chunk
        assert_eq!(chunks.len(), 1, "Small file should not be split, got {} chunks", chunks.len());
        assert!(score > 99.0, "Small file with one chunk should score ~100%, got {:.1}%", score);
    }
}
