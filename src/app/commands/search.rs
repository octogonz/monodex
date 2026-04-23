//! Handler for the `search` command.
//!
//! Edit here when: Modifying search output or result formatting.
//! Do not edit here for: Qdrant search queries (see `engine/uploader/search.rs`).

use crate::app::{Config, resolve_label_context, sanitize_for_terminal};
use crate::engine::{ParallelEmbedder, uploader::QdrantUploader};

pub fn run_search(
    config: &Config,
    text: &str,
    limit: usize,
    label: Option<&str>,
    catalog: Option<&str>,
    debug: bool,
) -> anyhow::Result<()> {
    // Resolve label context from explicit flags or default context
    let (label_id, catalog_name, label) = resolve_label_context(label, catalog)?;

    // Generate embedding for query
    let embedder = ParallelEmbedder::new()?;
    let embedding = embedder.encode(text, 0)?;

    // Query Qdrant with label filter
    let uploader = QdrantUploader::new(
        &config.qdrant.collection,
        config.qdrant.url.as_deref(),
        debug,
        config.qdrant.get_max_upload_bytes(),
    )?;

    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label);
    println!();

    let results =
        uploader.search_with_label(&embedding, limit, &catalog_name, label_id.as_str())?;

    // Display results as blurbs
    for result in &results {
        // Line 1: file_id:chunk_ordinal  score  breadcrumb [chunk_kind] (part N/M)
        // E.1: Sanitize breadcrumb to prevent terminal injection
        let breadcrumb =
            sanitize_for_terminal(result.payload.breadcrumb.as_deref().unwrap_or("unknown"));

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

        println!(
            "{}:{}  {:.3}  {}",
            result.payload.file_id, result.payload.chunk_ordinal, result.score, report
        );

        // Lines 2-4: first 3 lines of code (quoted with >)
        for line in result.payload.text.lines().take(3) {
            println!("> {}", line);
        }

        // Blank line between results
        println!();
    }

    Ok(())
}
