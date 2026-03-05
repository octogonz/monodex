//! Markdown partition-based chunking
//!
//! Simple markdown partitioner that splits at natural boundaries:
//! - Headings (#, ##, ###)
//! - Fenced code blocks (```)
//! - Block quotes (>>>)
//! - Paragraphs

#![allow(dead_code)]

/// Partition a Markdown file into chunks
pub fn partition_markdown(source: &str, config: &super::partitioner::PartitionConfig, file_path: &str, catalog: &str) -> Vec<super::partitioner::PartitionedChunk> {
    use super::partitioner::PartitionedChunk;
    use sha2::{Sha256, Digest};
    
    let lines: Vec<&str> = source.lines().collect();
    let mut chunks = Vec::new();
    
    // Compute content hash
    let content_hash = {
        let mut hasher = Sha256::new();
        hasher.update(source.as_bytes());
        format!("sha256:{:x}", hasher.finalize())
    };
    
    // Build breadcrumb prefix
    let breadcrumb_prefix = if config.package_name.is_empty() {
        config.file_name.clone()
    } else {
        format!("{}:{}", config.package_name, config.file_name)
    };
    
    // Find all section boundaries (headings)
    let mut section_starts: Vec<usize> = Vec::new();
    let mut in_code_block = false;
    
    for (i, line) in lines.iter().enumerate() {
        // Track code blocks
        if line.trim().starts_with("```") {
            in_code_block = !in_code_block;
        }
        
        // Skip headings inside code blocks
        if in_code_block {
            continue;
        }
        
        // Detect headings
        if line.trim().starts_with('#') {
            section_starts.push(i);
        }
    }
    
    // Add end boundary
    section_starts.push(lines.len());
    
    // If no sections, treat entire file as one chunk
    if section_starts.len() == 1 {
        let text = lines.join("\n");
        if !text.trim().is_empty() {
            chunks.push(PartitionedChunk {
                source_uri: file_path.to_string(),
                catalog: catalog.to_string(),
                content_hash: content_hash.clone(),
                breadcrumb: breadcrumb_prefix.clone(),
                text,
                start_line: 1,
                end_line: lines.len(),
                chunk_type: "markdown".to_string(),
                symbol_name: None,
            });
        }
        return chunks;
    }
    
    // Create chunks for each section
    for i in 0..section_starts.len() - 1 {
        let start_idx = section_starts[i];
        let end_idx = section_starts[i + 1];
        
        if start_idx >= end_idx {
            continue;
        }
        
        let section_lines = &lines[start_idx..end_idx];
        let section_text = section_lines.join("\n");
        
        // Skip empty sections
        if section_text.trim().is_empty() {
            continue;
        }
        
        // Get heading for breadcrumb
        let heading = extract_heading_text(section_lines[0]);
        let breadcrumb = if let Some(h) = &heading {
            format!("{}:{}", breadcrumb_prefix, h)
        } else {
            breadcrumb_prefix.clone()
        };
        
        // If section is oversized, split it further
        if section_text.len() > config.target_size {
            split_oversized_section(
                section_lines,
                start_idx + 1,  // 1-indexed
                config,
                &breadcrumb,
                file_path,
                catalog,
                &content_hash,
                &mut chunks,
            );
        } else {
            chunks.push(PartitionedChunk {
                source_uri: file_path.to_string(),
                catalog: catalog.to_string(),
                content_hash: content_hash.clone(),
                breadcrumb,
                text: section_text,
                start_line: start_idx + 1,  // 1-indexed
                end_line: end_idx,
                chunk_type: "section".to_string(),
                symbol_name: heading,
            });
        }
    }
    
    chunks
}

/// Extract heading text from a markdown heading line
fn extract_heading_text(line: &str) -> Option<String> {
    let trimmed = line.trim();
    
    // ATX-style heading (# Heading)
    if trimmed.starts_with('#') {
        let text = trimmed.trim_start_matches('#').trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    
    // Setext-style heading (underlined with === or ---)
    // Would need to look at previous line
    
    None
}

/// Split an oversized section into smaller chunks
fn split_oversized_section(
    lines: &[&str],
    start_line: usize,
    config: &super::partitioner::PartitionConfig,
    breadcrumb: &str,
    file_path: &str,
    catalog: &str,
    content_hash: &str,
    chunks: &mut Vec<super::partitioner::PartitionedChunk>,
) {
    use super::partitioner::PartitionedChunk;
    
    // Find split points within the section
    let mut split_points: Vec<(usize, usize)> = Vec::new();
    let mut current_start = 0;
    let mut current_size = 0;
    let mut in_code_block = false;
    
    for (i, line) in lines.iter().enumerate() {
        // Track code blocks
        if line.trim().starts_with("```") {
            in_code_block = !in_code_block;
        }
        
        let line_size = line.len() + 1;  // +1 for newline
        
        // Check if we should split here
        let should_split = current_size + line_size > config.target_size 
            && current_size > 0
            && !in_code_block
            && (line.trim().is_empty() 
                || line.trim().starts_with("```")
                || line.trim().starts_with('>')
                || line.trim().starts_with('-')
                || line.trim().starts_with('*'));
        
        if should_split {
            split_points.push((current_start, i));
            current_start = i;
            current_size = 0;
        }
        
        current_size += line_size;
    }
    
    // Add final chunk
    if current_start < lines.len() {
        split_points.push((current_start, lines.len()));
    }
    
    // If we only have one chunk and it's still oversized, split by lines
    if split_points.len() == 1 && lines.join("\n").len() > config.target_size {
        split_by_lines_fallback(lines, start_line, config, breadcrumb, file_path, catalog, content_hash, chunks);
        return;
    }
    
    // Emit chunks
    for (i, (start_idx, end_idx)) in split_points.iter().enumerate() {
        let chunk_lines = &lines[*start_idx..*end_idx];
        let chunk_text = chunk_lines.join("\n");
        
        if chunk_text.trim().is_empty() {
            continue;
        }
        
        chunks.push(PartitionedChunk {
            source_uri: file_path.to_string(),
            catalog: catalog.to_string(),
            content_hash: content_hash.to_string(),
            breadcrumb: format!("{} (part {}/{})", breadcrumb, i + 1, split_points.len()),
            text: chunk_text,
            start_line: start_line + start_idx,
            end_line: start_line + end_idx - 1,
            chunk_type: "section".to_string(),
            symbol_name: None,
        });
    }
}

/// Fallback: split by lines when other methods fail
fn split_by_lines_fallback(
    lines: &[&str],
    start_line: usize,
    config: &super::partitioner::PartitionConfig,
    breadcrumb: &str,
    file_path: &str,
    catalog: &str,
    content_hash: &str,
    chunks: &mut Vec<super::partitioner::PartitionedChunk>,
) {
    use super::partitioner::PartitionedChunk;
    
    let mut current_start = 0;
    let mut current_size = 0;
    let mut part_num = 1;
    
    for (i, line) in lines.iter().enumerate() {
        let line_size = line.len() + 1;
        
        if current_size + line_size > config.target_size && current_size > 0 {
            let chunk_lines = &lines[current_start..i];
            chunks.push(PartitionedChunk {
                source_uri: file_path.to_string(),
                catalog: catalog.to_string(),
                content_hash: content_hash.to_string(),
                breadcrumb: format!("{} (part {})", breadcrumb, part_num),
                text: chunk_lines.join("\n"),
                start_line: start_line + current_start,
                end_line: start_line + i - 1,
                chunk_type: "markdown".to_string(),
                symbol_name: None,
            });
            
            current_start = i;
            current_size = 0;
            part_num += 1;
        }
        
        current_size += line_size;
    }
    
    // Add final chunk
    if current_start < lines.len() {
        let chunk_lines = &lines[current_start..];
        chunks.push(PartitionedChunk {
            source_uri: file_path.to_string(),
            catalog: catalog.to_string(),
            content_hash: content_hash.to_string(),
            breadcrumb: format!("{} (part {})", breadcrumb, part_num),
            text: chunk_lines.join("\n"),
            start_line: start_line + current_start,
            end_line: start_line + lines.len() - 1,
            chunk_type: "markdown".to_string(),
            symbol_name: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::partitioner::{PartitionConfig, PartitionedChunk};
    use super::partition_markdown;
    use insta::assert_snapshot;
    
    fn format_chunks(chunks: &[PartitionedChunk]) -> String {
        let mut result = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            result.push_str(&format!(
                "=== CHUNK {} ===\nBreadcrumb: {}\nType: {}\nLines: {}-{}\nSize: {} chars\nPreview:\n{}\n\n",
                i + 1,
                chunk.breadcrumb,
                chunk.chunk_type,
                chunk.start_line,
                chunk.end_line,
                chunk.text.len(),
                chunk.text.lines().take(6).collect::<Vec<_>>().join("\n")
            ));
        }
        result
    }
    
    #[test]
    fn test_markdown_simple() {
        let source = r#"# Main Title

This is intro paragraph.

## Section 1

Some text here.

### Subsection

More content.

## Section 2

Final paragraph.
"#;
        
        let config = PartitionConfig {
            file_name: "test.md".to_string(),
            package_name: "@test/docs".to_string(),
            ..Default::default()
        };
        
        let chunks = partition_markdown(source, &config, "test.md", "test");
        assert_snapshot!(format_chunks(&chunks));
    }
    
    #[test]
    fn test_markdown_with_code() {
        let source = include_str!("../../test_artifacts/test.md");
        let config = PartitionConfig {
            file_name: "API.md".to_string(),
            package_name: "rush-qdrant".to_string(),
            ..Default::default()
        };
        
        let chunks = partition_markdown(source, &config, "API.md", "test");
        assert_snapshot!(format_chunks(&chunks));
    }
}
