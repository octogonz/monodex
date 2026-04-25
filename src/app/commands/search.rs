//! Handler for the `search` command.
//!
//! Edit here when: Modifying search output or result formatting.
//! Do not edit here for: Vector search logic (see `engine/storage/chunks.rs`).

use crate::app::{Config, resolve_database_path, resolve_label_context, sanitize_for_terminal};
use crate::engine::{ParallelEmbedder, storage::Database};

pub fn run_search(
    config: &Config,
    text: &str,
    limit: usize,
    label: Option<&str>,
    catalog: Option<&str>,
    _debug: bool,
) -> anyhow::Result<()> {
    // Resolve label context from explicit flags or default context
    let (label_id, catalog_name, label) = resolve_label_context(label, catalog)?;

    // Open database (handshake validates monodex-meta.json)
    let db_path = resolve_database_path(Some(config))?;
    let rt = tokio::runtime::Runtime::new()?;
    let results = rt.block_on(async {
        let db = Database::open(&db_path).await?;
        let chunk_storage = db.chunks_storage().await?;

        // Generate embedding for query
        let embedder = ParallelEmbedder::new()?;
        let embedding = embedder.encode(text, 0)?;

        // Query LanceDB with label filter
        chunk_storage.vector_search(&embedding, label_id.as_str(), limit).await
    })?;

    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label);
    println!();

    // Display results as blurbs
    for result in &results {
        let chunk = &result.chunk;

        // Line 1: file_id:chunk_ordinal  score  breadcrumb [chunk_kind] (part N/M)
        // E.1: Sanitize breadcrumb to prevent terminal injection
        let breadcrumb =
            sanitize_for_terminal(chunk.breadcrumb.as_deref().unwrap_or("unknown"));

        // Build the report form with chunk_kind and split metadata
        let mut report = breadcrumb.clone();
        if let (Some(ordinal), Some(count)) = (
            chunk.split_part_ordinal,
            chunk.split_part_count,
        ) {
            report = format!("{} (part {}/{})", report, ordinal, count);
        }
        if chunk.chunk_kind != "content" {
            report = format!("{} [{}]", report, chunk.chunk_kind);
        }

        println!(
            "{}:{}  {:.3}  {}",
            chunk.file_id, chunk.chunk_ordinal, result.score, report
        );

        // Lines 2-4: first 3 lines of code (quoted with >)
        for line in chunk.text.lines().take(3) {
            println!("> {}", line);
        }

        // Blank line between results
        println!();
    }

    Ok(())
}
