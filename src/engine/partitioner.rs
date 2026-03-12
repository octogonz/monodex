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
//! - Recursively walks the syntax tree using scope-based traversal
//! - Provides candidate split points as line numbers
//! - "Meaningful" = doesn't break semantic units
//!
//! **Scope-Based Traversal:**
//! - Split scopes: nodes whose direct children define split boundaries
//!   (program, class_body, statement_block, switch_body)
//! - Transparent conduits: wrapper nodes to pass through when descending
//!   (if_statement, function_declaration, return_statement, etc.)
//! - Core rule: choose the shallowest split scope that yields a usable partition
//!
//! **Minimum Size Constraints:**
//! - Minimum chunk size: 20% of target (prevents tiny fragments)
//! - Tiny nestedscopes (<30 lines) are filtered out as split candidates
//! - Large expression statements (>500 bytes) are treated as meaningful
//!
//! **Coordination:**
//! 1. Start with one chunk = entire file
//! 2. While any chunk exceeds budget:
//!    a. Find the shallowest split scope spanning the chunk
//!    b. Get candidate boundaries from that scope's direct children
//!    c. If usable split found, divide the chunk
//!    d. Otherwise, descend through transparent conduits to nested scopes
//!    e. If no AST split works, fall back to line-based splitting
//! 3. Done - all chunks fit budget

use tree_sitter::{Node, Parser};
use super::util::compute_hash;

/// Target chunk size in lines (derived from 6000 char target, ~50 chars/line)
const TARGET_LINES: usize = 120;

/// Minimum chunk size as ratio of target (20%)
const MIN_CHUNK_RATIO: f64 = 0.20;

/// Debug logging for partitioning decisions
#[derive(Debug, Clone, Copy, Default)]
pub struct PartitionDebug {
    /// Enable verbose logging of split decisions
    pub enabled: bool,
}

impl PartitionDebug {
    pub fn log(&self, msg: &str) {
        if self.enabled {
            eprintln!("[DEBUG] {}", msg);
        }
    }
    
    pub fn log_split_attempt(&self, start_line: usize, end_line: usize, chunk_size: usize) {
        if self.enabled {
            eprintln!("[DEBUG] === Splitting chunk lines {}-{} ({} chars) ===", start_line, end_line, chunk_size);
        }
    }
    
    pub fn log_scope(&self, scope_type: &str, kind: &str, start_line: usize, end_line: usize) {
        if self.enabled {
            eprintln!("[DEBUG] {} scope '{}' at lines {}-{}", scope_type, kind, start_line, end_line);
        }
    }
    
    pub fn log_candidates(&self, candidates: &[usize]) {
        if self.enabled {
            eprintln!("[DEBUG]   Candidates: {:?}", candidates);
        }
    }
    
    pub fn log_split_decision(&self, result: &str, split_line: Option<usize>) {
        if self.enabled {
            match split_line {
                Some(line) => eprintln!("[DEBUG]   => {} at line {}", result, line),
                None => eprintln!("[DEBUG]   => {}", result),
            }
        }
    }
    
    pub fn log_meaningful_child(&self, kind: &str, start_line: usize, end_line: usize) {
        if self.enabled {
            eprintln!("[DEBUG]   Meaningful child: '{}' at lines {}-{}", kind, start_line, end_line);
        }
    }
}

/// Check if a node is a split scope - direct children define split boundaries.
fn is_split_scope(kind: &str) -> bool {
    matches!(kind,
        "program" | "source_file" |
        "class_body" | "declaration_list" | "object_type" |
        "statement_block" | "switch_body"
    )
}

/// Check if a node is a transparent conduit - pass through to nested scopes.
fn is_transparent_conduit(kind: &str) -> bool {
    matches!(kind,
        "export_statement" |
        // Declaration containers (hold class_body, object_type, etc.)
        "class_declaration" | "abstract_class_declaration" | "interface_declaration" |
        "type_alias_declaration" | "enum_declaration" |
        "function_declaration" | "method_definition" | "arrow_function" |
        "if_statement" | "try_statement" | "catch_clause" |
        "for_statement" | "for_in_statement" | "for_of_statement" |
        "while_statement" | "do_statement" |
        "switch_statement" | "switch_case" |
        "return_statement" | "throw_statement" |
        "expression_statement" |
        // Expression wrappers that may contain nested scopes
        "await_expression" | "new_expression" | "arguments" | "call_expression"
    )
}

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
    
    /// Debug logging for partitioning decisions
    pub debug: PartitionDebug,
}

impl Default for PartitionConfig {
    fn default() -> Self {
        Self {
            target_size: 6000,
            file_name: "unknown.ts".to_string(),
            package_name: "unknown".to_string(),
            debug: PartitionDebug::default(),
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

/// Result of attempting to split a chunk
enum SplitResult {
    /// Found a valid AST-based split at this line
    Split(usize),
    /// No valid AST split, using fallback line-based split
    Fallback(usize),
    /// Cannot split this chunk any further
    CannotSplit,
}

/// Find the best split point for an oversized chunk using scope-based approach.
fn find_best_split(
    root: Node,
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    target_size: usize,
    min_chunk_size: usize,
    source: &[u8],
    debug: &PartitionDebug,
) -> SplitResult {
    debug.log_split_attempt(start_line, end_line, chunk_size);
    
    // Find the shallowest split scope that spans this chunk
    let initial_scope = find_shallowest_split_scope(root, start_line, end_line, source, debug);
    debug.log_scope("Initial", initial_scope.kind(), 
        initial_scope.start_position().row + 1, 
        initial_scope.end_position().row + 1);
    
    // Track the least-bad AST split seen during descent
    let mut least_bad_split: Option<(usize, usize)> = None;
    let ideal_first_size = chunk_size / 2;
    
    // Walk the chain of nested split scopes
    let mut current_scope = Some(initial_scope);
    let mut iteration = 0;
    
    while let Some(scope) = current_scope {
        iteration += 1;
        debug.log(&format!("--- Iteration {} ---", iteration));
        
        // Get candidate boundaries from this scope's direct children
        let candidates = get_scope_candidates(scope, source, start_line, end_line, debug);
        debug.log_candidates(&candidates);
        
        if !candidates.is_empty() {
            // Check if this scope yields a usable partition
            if let Some(split_line) = find_usable_split(
                &candidates, start_line, end_line, chunk_size, target_size, min_chunk_size,
            ) {
                debug.log_split_decision("USABLE SPLIT", Some(split_line));
                return SplitResult::Split(split_line);
            }
            debug.log_split_decision("No usable split (min_chunk constraint)", None);
            
            // No usable partition - record the least-bad split from this scope
            // But only if it respects min_chunk_size (don't record tiny splits)
            for &split_line in &candidates {
                let lines_before = split_line - start_line + 1;
                let total_lines = end_line - start_line + 1;
                let estimated_first_size = (chunk_size * lines_before) / total_lines;
                let estimated_second_size = chunk_size - estimated_first_size;
                
                // Compute badness for this split
                // Note: We include ALL candidates, even those that create tiny chunks,
                // because a semantically-meaningful split is better than line-based fallback.
                // The badness calculation already penalizes small chunks heavily.
                let badness = compute_split_badness(
                    split_line, start_line, end_line, chunk_size, ideal_first_size,
                );
                
                // Log the candidate with size info
                let size_note = if estimated_first_size < min_chunk_size || estimated_second_size < min_chunk_size {
                    format!(" (WARNING: creates tiny chunk)")
                } else {
                    String::new()
                };
                debug.log(&format!("  Candidate line {} -> sizes ({}, {}) badness {}{}", 
                    split_line, estimated_first_size, estimated_second_size, badness, size_note));
                
                if least_bad_split.map_or(true, |(_, b)| badness < b) {
                    least_bad_split = Some((split_line, badness));
                }
            }
        }
        
        // Descend to a nested split scope
        current_scope = find_nested_split_scope(scope, start_line, end_line, source, debug);
    }
    
    // No usable partition found - use least-bad AST split if available
    if let Some((split_line, badness)) = least_bad_split {
        debug.log_split_decision(&format!("Least-bad AST split (badness {})", badness), Some(split_line));
        return SplitResult::Split(split_line);
    }
    
    // No AST split at all - use line-based fallback
    let mid_line = start_line + (end_line - start_line) / 2;
    if mid_line > start_line && mid_line < end_line {
        debug.log_split_decision("FALLBACK (no AST candidates)", Some(mid_line));
        SplitResult::Fallback(mid_line)
    } else {
        debug.log_split_decision("CANNOT SPLIT (too small)", None);
        SplitResult::CannotSplit
    }
}

/// Find the shallowest split scope that spans the given line range.
fn find_shallowest_split_scope<'a>(
    node: Node<'a>,
    start_line: usize,
    end_line: usize,
    source: &[u8],
    debug: &PartitionDebug,
) -> Node<'a> {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;
    
    if node_start > start_line || node_end < end_line {
        return node;
    }
    
    // If this is a split scope, return it (shallowest wins)
    if is_split_scope(node.kind()) {
        return node;
    }
    
    // If transparent conduit, pass through to the child that spans the range
    if is_transparent_conduit(node.kind()) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_start = child.start_position().row + 1;
            let child_end = child.end_position().row + 1;
            if child_start <= start_line && child_end >= end_line {
                return find_shallowest_split_scope(child, start_line, end_line, source, debug);
            }
        }
    }
    
    // Check for single meaningful child to descend into
    let meaningful = get_meaningful_children(node, source, debug);
    if meaningful.len() == 1 {
        let child = meaningful[0];
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;
        if child_start <= start_line && child_end >= end_line {
            return find_shallowest_split_scope(child, start_line, end_line, source, debug);
        }
    }
    
    node
}

/// Find a nested split scope to descend into.
/// Recursively descends through chains of transparent conduits until finding a split scope.
fn find_nested_split_scope<'a>(
    node: Node<'a>,
    start_line: usize,
    end_line: usize,
    source: &[u8],
    debug: &PartitionDebug,
) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let mut best_child: Option<Node<'a>> = None;
    let mut best_score = i32::MIN;
    
    for child in node.children(&mut cursor) {
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;
        
        // Must overlap the chunk range
        if child_end < start_line || child_start > end_line {
            continue;
        }
        
        // Check if this child is a split scope or transparent conduit
        if is_split_scope(child.kind()) || is_transparent_conduit(child.kind()) {
            let overlap_start = child_start.max(start_line);
            let overlap_end = child_end.min(end_line);
            let overlap = (overlap_end - overlap_start) as i32;
            
            // Prefer children that are split scopes and centered in the range
            let center_score = if is_split_scope(child.kind()) { 1000 } else { 0 };
            let total_score = center_score + overlap;
            
            if total_score > best_score {
                best_score = total_score;
                best_child = Some(child);
            }
        }
    }
    
    // If we found a transparent conduit, recursively descend through it
    // to find a nested split scope
    if let Some(child) = best_child {
        debug.log(&format!("Descending to child '{}' at lines {}-{}", 
            child.kind(), child.start_position().row + 1, child.end_position().row + 1));
        if is_transparent_conduit(child.kind()) {
            // Try to find a deeper split scope inside this conduit
            if let Some(deeper) = find_deepest_split_scope(child, start_line, end_line, source, debug) {
                return Some(deeper);
            }
        }
    }
    
    best_child
}

/// Find a usable split scope within a transparent conduit chain.
/// Descends through layers of transparent conduits until finding a split scope
/// that has meaningful children in the given line range.
/// 
/// Key constraint: the scope must be substantial (≥30 lines) to filter out
/// tiny nested functions/callbacks that would create micro-chunks.
fn find_deepest_split_scope<'a>(
    node: Node<'a>,
    start_line: usize,
    end_line: usize,
    source: &[u8],
    debug: &PartitionDebug,
) -> Option<Node<'a>> {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;
    
    // Must overlap the chunk range
    if node_end < start_line || node_start > end_line {
        return None;
    }
    
    // If this is a split scope, check if it has meaningful children
    if is_split_scope(node.kind()) {
        let children = get_meaningful_children(node, source, debug);
        let overlapping: Vec<Node> = children
            .into_iter()
            .filter(|child| {
                let child_start = child.start_position().row + 1;
                let child_end = child.end_position().row + 1;
                child_end >= start_line && child_start <= end_line
            })
            .collect();
        
        // If it has 2+ overlapping meaningful children, check if this scope
        // overlaps the chunk range substantially (at least 30 lines or 80% of the scope)
        if overlapping.len() >= 2 {
            let overlap_start = node_start.max(start_line);
            let overlap_end = node_end.min(end_line);
            let overlap_lines = overlap_end - overlap_start + 1;
            let scope_lines = node_end - node_start + 1;
            
            debug.log(&format!("  Split scope '{}' has {} overlapping children, scope_lines={}, overlap_lines={}",
                node.kind(), overlapping.len(), scope_lines, overlap_lines));
            
            // Only accept if the scope overlaps at least 30 lines AND
            // the scope is at least 30 lines (to filter out tiny nested scopes)
            if overlap_lines >= 30 && scope_lines >= 30 {
                debug.log(&format!("  -> Accepted split scope '{}' at lines {}-{}", 
                    node.kind(), node_start, node_end));
                return Some(node);
            } else {
                debug.log(&format!("  -> Rejected: overlap_lines ({}) < 30 or scope_lines ({}) < 30", 
                    overlap_lines, scope_lines));
            }
        }
    }
    
    // If transparent conduit or split scope with insufficient children, descend further
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;
        
        if child_end < start_line || child_start > end_line {
            continue;
        }
        
        // Recursively descend into transparent conduits or split scopes
        if is_transparent_conduit(child.kind()) || is_split_scope(child.kind()) {
            if let Some(deeper) = find_deepest_split_scope(child, start_line, end_line, source, debug) {
                return Some(deeper);
            }
        }
    }
    
    None
}

/// Get candidate split boundaries from a scope's direct children.
fn get_scope_candidates(
    scope: Node,
    source: &[u8],
    chunk_start: usize,
    chunk_end: usize,
    debug: &PartitionDebug,
) -> Vec<usize> {
    let children = get_meaningful_children(scope, source, debug);
    
    let overlapping: Vec<Node> = children
        .into_iter()
        .filter(|child| {
            let child_start = child.start_position().row + 1;
            let child_end = child.end_position().row + 1;
            child_end >= chunk_start && child_start <= chunk_end
        })
        .collect();
    
    if overlapping.len() < 2 {
        return Vec::new();
    }
    
    let mut candidates: Vec<usize> = Vec::new();
    for i in 1..overlapping.len() {
        let prev_child = overlapping[i - 1];
        let split_line = prev_child.end_position().row + 1;
        if split_line >= chunk_start && split_line < chunk_end {
            candidates.push(split_line);
        }
    }
    candidates
}

/// Find a usable split from the candidates.
fn find_usable_split(
    candidates: &[usize],
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    _target_size: usize,
    min_chunk_size: usize,
) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    
    let ideal_first_size = chunk_size / 2;
    let mut best_split: Option<usize> = None;
    let mut best_distance = usize::MAX;
    
    for &split_line in candidates {
        let lines_before = split_line - start_line + 1;
        let total_lines = end_line - start_line + 1;
        let estimated_first_size = (chunk_size * lines_before) / total_lines;
        let estimated_second_size = chunk_size - estimated_first_size;
        
        // Skip splits that create tiny chunks (below minimum size)
        // Note: We don't require chunks to be below target_size here,
        // because the greedy loop will split oversized chunks in subsequent iterations.
        if estimated_first_size < min_chunk_size || estimated_second_size < min_chunk_size {
            continue;
        }
        
        // Prefer splits closest to middle
        let distance = if estimated_first_size > ideal_first_size {
            estimated_first_size - ideal_first_size
        } else {
            ideal_first_size - estimated_first_size
        };
        
        if distance < best_distance {
            best_distance = distance;
            best_split = Some(split_line);
        }
    }
    best_split
}

/// Compute badness score for a split (lower is better).
fn compute_split_badness(
    split_line: usize,
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    ideal_first_size: usize,
) -> usize {
    let lines_before = split_line - start_line + 1;
    let total_lines = end_line - start_line + 1;
    let estimated_first_size = (chunk_size * lines_before) / total_lines;
    
    let distance = if estimated_first_size > ideal_first_size {
        estimated_first_size - ideal_first_size
    } else {
        ideal_first_size - estimated_first_size
    };
    
    // Add penalty for small chunks
    let estimated_second_size = chunk_size - estimated_first_size;
    let tiny_penalty = if estimated_first_size < 500 || estimated_second_size < 500 {
        10000
    } else if estimated_first_size < 1000 || estimated_second_size < 1000 {
        5000
    } else {
        0
    };
    
    distance + tiny_penalty
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
    let min_chunk_size = (config.target_size as f64 * MIN_CHUNK_RATIO) as usize;
    let mut used_fallback_split = false;
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
                // Chunk is too big - use scope-based splitting
                match find_best_split(
                    root,
                    chunk_range.start_line,
                    chunk_range.end_line,
                    chunk_size,
                    config.target_size,
                    min_chunk_size,
                    source.as_bytes(),
                    &config.debug,
                ) {
                    SplitResult::Split(split_line) => {
                        new_chunks.push(ChunkRange { start_line: chunk_range.start_line, end_line: split_line });
                        new_chunks.push(ChunkRange { start_line: split_line + 1, end_line: chunk_range.end_line });
                        changed = true;
                    }
                    SplitResult::Fallback(split_line) => {
                        used_fallback_split = true;
                        new_chunks.push(ChunkRange { start_line: chunk_range.start_line, end_line: split_line });
                        new_chunks.push(ChunkRange { start_line: split_line + 1, end_line: chunk_range.end_line });
                        changed = true;
                    }
                    SplitResult::CannotSplit => {
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
        
        let mut breadcrumb = if breadcrumb_suffix.is_empty() {
            base_breadcrumb.clone()
        } else {
            format!("{}:{}", base_breadcrumb, breadcrumb_suffix)
        };

        if used_fallback_split {
            breadcrumb.push_str(":[fallback-split]");
        }
        
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
/// Strategy: pick the split closest to the middle of the chunk (in characters)
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
    
    // Target: split at approximately half the chunk size
    let ideal_first_size = chunk_size / 2;
    
    // Find the split point that creates a first chunk closest to ideal
    // but not exceeding target_size
    let mut best_split: Option<usize> = None;
    let mut best_distance = usize::MAX;
    
    for &split_line in split_points {
        let lines_before = split_line - start_line + 1;
        let total_lines = end_line - start_line + 1;
        let estimated_first_size = (chunk_size * lines_before) / total_lines;
        
        // Only consider splits that don't exceed target
        if estimated_first_size > target_size {
            continue;
        }
        
        // Prefer splits that get closest to half the chunk size
        let distance = if estimated_first_size > ideal_first_size {
            estimated_first_size - ideal_first_size
        } else {
            ideal_first_size - estimated_first_size
        };
        
        if distance < best_distance {
            best_distance = distance;
            best_split = Some(split_line);
        }
    }
    
    best_split
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
fn get_meaningful_children<'a>(node: Node<'a>, source: &[u8], debug: &PartitionDebug) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    let mut children = Vec::new();
    
    for child in node.children(&mut cursor) {
        if is_meaningful_split_point(child, source) {
            debug.log_meaningful_child(
                child.kind(), 
                child.start_position().row + 1, 
                child.end_position().row + 1
            );
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
fn extract_imports_end_line(root: Node, _source: &[u8]) -> usize {
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

/// Determine if an AST node represents a meaningful split boundary.
///
/// Meaningful nodes are those that can serve as split points between chunks.
/// This includes declarations (functions, classes, interfaces, etc.) and
/// large expression statements (e.g., event handler registrations).
///
/// Small nodes (like 1-line variable declarations) are technically meaningful
/// but may be filtered out later if they would create tiny chunks.
fn is_meaningful_split_point(node: Node, source: &[u8]) -> bool {
    match node.kind() {
        "function_declaration" | "class_declaration" | "interface_declaration" |
        "type_alias_declaration" | "enum_declaration" => true,
        
        "export_statement" => true,
        
        "method_definition" => true,
        
        "public_field_definition" | "property_declaration" => {
            node.end_byte() - node.start_byte() > 50
        }
        
        "lexical_declaration" | "variable_declaration" => {
            let text = String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]);
            text.starts_with("const") || text.starts_with("let") || text.starts_with("var")
        }
        
        // Expression statements are meaningful if they're large enough
        // (e.g., event handlers like `ws.on('connection', ...)`)
        "expression_statement" => {
            node.end_byte() - node.start_byte() > 500
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
        
        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("simple_function_visualization", visualization);
        
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("simple_function_summary", summary);
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
        
        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("class_with_methods_visualization", visualization);
        
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("class_with_methods_summary", summary);
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
        
        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("jsonfile_visualization", visualization);
        
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("jsonfile_summary", summary);
    }
    
    #[test]
    fn test_small_file_not_penalized() {
        // A small file with one chunk should score 100%
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
        
        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("small_file_visualization", visualization);
        
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("small_file_summary", summary);
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
        
        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("rollup_visualization", visualization);
        
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("rollup_summary", summary);
    }
    
    #[test]
    fn test_tunneled_browser_connection() {
        // A 231-line file with nested functions that produces tiny chunks
        // This is a regression test for the "tiny chunks for variables" bug
        let source = include_str!("../../test_artifacts/TunneledBrowserConnection.ts");
        let config = PartitionConfig {
            file_name: "TunneledBrowserConnection.ts".to_string(),
            package_name: "@rushstack/playwright-browser-tunnel".to_string(),
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "TunneledBrowserConnection.ts", "playwright-browser-tunnel");
        
        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("tunneled_visualization", visualization);
        
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("tunneled_summary", summary);
    }
    
    #[test]
    fn test_long_template_string_fallback() {
        // A degenerate case: a long template literal that cannot be split by AST
        // The chunker should fall back to line-based splitting with a warning
        // We make it 200 lines to exceed the 6000 char target
        let mut source_lines = vec![
            "// A file with a very long template string".to_string(),
            "const longString = `".to_string(),
        ];
        for i in 1..=200 {
            source_lines.push(format!("line{} some content here to make it longer", i));
        }
        source_lines.push("`;".to_string());
        source_lines.push("console.log(longString);".to_string());
        let source = source_lines.join("\n");
        
        let config = PartitionConfig {
            file_name: "long_string.ts".to_string(),
            package_name: "test".to_string(),
            ..Default::default()
        };
        let chunks = partition_typescript(&source, &config, "long_string.ts", "test");
        
        let visualization = format_chunks_visualization(&source, &chunks);
        assert_snapshot!("long_string_visualization", visualization);
        
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("long_string_summary", summary);
        
        // Verify that the long string was split despite having no AST split points
        assert!(chunks.len() > 1, "Expected fallback to split the oversized chunk");
        
        // Verify no chunk exceeds target size (with some tolerance for the fallback)
        for chunk in &chunks {
            assert!(chunk.text.len() <= config.target_size + 500, 
                "Chunk at lines {}-{} exceeds target: {} chars", 
                chunk.start_line, chunk.end_line, chunk.text.len());
        }
    }

    #[test]
    fn test_colorize_class_with_enum() {
        // A 289-line file (8031 chars) with an enum and a class with many methods
        // This tests the ability to split a class into method-level chunks
        let source = include_str!("../../test_artifacts/Colorize.ts");
        let config = PartitionConfig {
            file_name: "Colorize.ts".to_string(),
            package_name: "@rushstack/terminal".to_string(),
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "Colorize.ts", "terminal");
        
        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("colorize_visualization", visualization);
        
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("colorize_summary", summary);
        
        // Should NOT have fallback split - should use AST-based splitting at method boundaries
        for chunk in &chunks {
            assert!(!chunk.breadcrumb.contains("[fallback-split]"), 
                "Unexpected fallback split in chunk: {}", chunk.breadcrumb);
        }
    }
    
    #[test]
    fn test_ipackagejson_interface_file() {
        // An interface-only file with large interfaces
        // Tests that interface boundaries are used as split points
        let source = include_str!("../../test_artifacts/IPackageJson.ts");
        let config = PartitionConfig {
            file_name: "IPackageJson.ts".to_string(),
            package_name: "@rushstack/node-core-library".to_string(),
            debug: PartitionDebug { enabled: true },
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "IPackageJson.ts", "node-core-library");
        
        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("ipackagejson_visualization", visualization);
        
        let summary = format_chunks_summary(&chunks, source.lines().count());
        assert_snapshot!("ipackagejson_summary", summary);
        
        // Should not need fallback split for interface files
        for chunk in &chunks {
            assert!(!chunk.breadcrumb.contains("[fallback-split]"), 
                "Unexpected fallback split in chunk: {}", chunk.breadcrumb);
        }
    }
    
}
