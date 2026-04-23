//! Handler for the `view` command.
//!
//! Edit here when: Modifying view output or chunk selector parsing.
//! Do not edit here for: Chunk retrieval (see `engine/uploader/search.rs`).

use std::collections::HashSet;

use crate::app::{Config, resolve_label_context, sanitize_for_terminal};
use crate::engine::uploader::{PointResult, QdrantUploader};

/// Parsed selector for file-based chunk queries
#[derive(Debug, Clone)]
enum ChunkSelector {
    /// All chunks in the file
    All,
    /// Single chunk at position N (1-indexed)
    Single(usize),
    /// Range from start to end (inclusive, 1-indexed)
    Range(usize, usize),
    /// Range from start to the end of file
    ToEnd(usize),
}

/// Parse file ID with optional selector
///
/// Formats:
/// - `700a4ba232fe9ddc` - all chunks in file
/// - `700a4ba232fe9ddc:3` - chunk 3
/// - `700a4ba232fe9ddc:2-3` - chunks 2 through 3
/// - `700a4ba232fe9ddc:3-end` - chunk 3 through the last chunk
fn parse_file_id_with_selector(s: &str) -> anyhow::Result<(String, ChunkSelector)> {
    let s = s.trim();

    // Check for selector suffix
    if let Some(colon_pos) = s.find(':') {
        let file_id = s[..colon_pos].to_string();
        let selector = &s[colon_pos + 1..];

        // Validate file_id is 16 hex chars
        if file_id.len() != 16 || !file_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                file_id
            ));
        }

        // Parse selector
        if selector == "end" {
            // Invalid: ":end" without start
            Err(anyhow::anyhow!(
                "Invalid selector ':end'. Use ':N-end' format."
            ))
        } else if let Some(start_str) = selector.strip_suffix("-end") {
            // :N-end format
            let start: usize = start_str
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if start < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    start
                ));
            }
            Ok((file_id, ChunkSelector::ToEnd(start)))
        } else if selector.contains('-') {
            // :N-M format
            let parts: Vec<&str> = selector.split('-').collect();
            if parts.len() != 2 {
                return Err(anyhow::anyhow!(
                    "Invalid selector '{}'. Expected ':N-M' format.",
                    selector
                ));
            }
            let start: usize = parts[0]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid start chunk in selector '{}'", selector))?;
            let end: usize = parts[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid end chunk in selector '{}'", selector))?;
            if start < 1 || end < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}:{}",
                    start,
                    end
                ));
            }
            if start > end {
                return Err(anyhow::anyhow!("Start chunk {} > end chunk {}", start, end));
            }
            Ok((file_id, ChunkSelector::Range(start, end)))
        } else {
            // :N format (single chunk)
            let chunk_num: usize = selector
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if chunk_num < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    chunk_num
                ));
            }
            Ok((file_id, ChunkSelector::Single(chunk_num)))
        }
    } else {
        // No selector - validate file_id and return All selector
        if s.len() != 16 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                s
            ));
        }
        Ok((s.to_string(), ChunkSelector::All))
    }
}

pub fn run_view(
    config: &Config,
    id_specs: &[String],
    label: Option<&str>,
    catalog: Option<&str>,
    show_full_paths: bool,
    chunks_only: bool,
    debug: bool,
) -> anyhow::Result<()> {
    if id_specs.is_empty() {
        return Err(anyhow::anyhow!(
            "No IDs provided. Use --id <file_id>[:<selector>]"
        ));
    }

    // Resolve label context from explicit flag or default context
    let (label_id, catalog_name, label) = resolve_label_context(label, catalog)?;

    // Parse all file IDs with selectors
    let mut requests: Vec<(String, ChunkSelector)> = Vec::new();
    for spec in id_specs {
        let (file_id, selector) = parse_file_id_with_selector(spec)?;
        requests.push((file_id, selector));
    }

    // Query Qdrant
    let uploader = QdrantUploader::new(
        &config.qdrant.collection,
        config.qdrant.url.as_deref(),
        debug,
        config.qdrant.get_max_upload_bytes(),
    )?;

    if !chunks_only {
        println!("Catalog: {}", catalog_name);
        println!("Label: {}", label);
        println!();
    }

    // Collect all results with their original selectors for display
    let mut all_results: Vec<(String, ChunkSelector, Vec<PointResult>)> = Vec::new();

    for (file_id, selector) in requests {
        let chunks = uploader.get_chunks_by_file_id_with_label(&file_id, label_id.as_str())?;

        // Filter by selector
        let filtered: Vec<PointResult> = match &selector {
            ChunkSelector::All => chunks,
            ChunkSelector::Single(n) => chunks
                .into_iter()
                .filter(|c| c.payload.chunk_ordinal == *n)
                .collect(),
            ChunkSelector::Range(start, end) => chunks
                .into_iter()
                .filter(|c| c.payload.chunk_ordinal >= *start && c.payload.chunk_ordinal <= *end)
                .collect(),
            ChunkSelector::ToEnd(start) => chunks
                .into_iter()
                .filter(|c| c.payload.chunk_ordinal >= *start)
                .collect(),
        };

        all_results.push((file_id, selector, filtered));
    }

    // Collect unique catalogs for preamble
    if !chunks_only {
        let catalogs: HashSet<&str> = all_results
            .iter()
            .flat_map(|(_, _, results)| results.iter().map(|r| r.payload.catalog.as_str()))
            .collect();

        if !catalogs.is_empty() {
            println!("Catalogs:");
            for cat in catalogs {
                if let Some(cat_config) = config.catalogs.get(cat) {
                    // E.1: Sanitize catalog name and path
                    println!("- {}", sanitize_for_terminal(cat));
                    println!(
                        "  Catalog path: {}",
                        sanitize_for_terminal(&cat_config.path)
                    );
                }
            }
            println!();
        }
    }

    // Display results
    for (file_id, selector, results) in &all_results {
        if results.is_empty() {
            // No chunks found
            let selector_str = match selector {
                ChunkSelector::All => String::new(),
                ChunkSelector::Single(n) => format!(":{}", n),
                ChunkSelector::Range(start, end) => format!(":{}-{}", start, end),
                ChunkSelector::ToEnd(start) => format!(":{}-end", start),
            };
            println!("{}{} ERROR: CHUNK NOT FOUND", file_id, selector_str);
            continue;
        }

        for result in results {
            // E.1: Sanitize output fields to prevent terminal injection
            let breadcrumb =
                sanitize_for_terminal(result.payload.breadcrumb.as_deref().unwrap_or("unknown"));
            let chunk_count = result.payload.chunk_count;
            let chunk_ordinal = result.payload.chunk_ordinal;

            // Build the report form with chunk_kind and split metadata
            let mut report = breadcrumb.clone();
            if let (Some(ordinal), Some(count)) = (
                result.payload.split_part_ordinal,
                result.payload.split_part_count,
            ) {
                report = format!("{} (part {}/{})", report, ordinal, count);
            }
            if result.payload.chunk_kind != "content" {
                report = format!("{} [{}]", report, result.payload.chunk_kind);
            }

            // Header line: <file_id>:<chunk_ordinal> (<n>/<total>) <breadcrumb> [kind] (part N/M)
            println!(
                "{}:{} ({}/{}) {}",
                file_id, chunk_ordinal, chunk_ordinal, chunk_count, report
            );

            // Source line (non-grammar format per spec §8.6)
            println!(
                "Source: ({}) {}",
                sanitize_for_terminal(&result.payload.catalog),
                sanitize_for_terminal(&result.payload.relative_path)
            );

            // Full path (optional)
            if show_full_paths {
                println!(
                    "Full path: {}",
                    sanitize_for_terminal(&result.payload.source_uri)
                );
            }

            // Lines and type
            println!(
                "Lines: {}-{}",
                result.payload.start_line, result.payload.end_line
            );
            println!(
                "Type: {}",
                sanitize_for_terminal(&result.payload.chunk_type)
            );

            // Content
            println!();
            for line in result.payload.text.lines() {
                println!("> {}", line);
            }

            println!();
        }
    }

    Ok(())
}
