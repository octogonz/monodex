//! Handler for the `purge` command.
//!
//! Edit here when: Modifying purge behavior (delete catalog or entire collection).
//! Do not edit here for: Storage delete operations (see `engine/storage/chunks.rs`, `engine/storage/labels.rs`).

use crate::app::{Config, resolve_database_path};
use crate::engine::storage::Database;

/// Run purge command (delete all chunks from a catalog or entire collection)
pub fn run_purge(
    config: &Config,
    catalog: Option<&str>,
    all: bool,
    _debug: bool,
) -> anyhow::Result<()> {
    // Open database (handshake validates monodex-meta.json)
    let db_path = resolve_database_path(Some(config))?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_purge_async(&db_path, catalog, all))
}

async fn run_purge_async(
    db_path: &std::path::Path,
    catalog: Option<&str>,
    all: bool,
) -> anyhow::Result<()> {
    let db = Database::open(db_path).await?;
    let chunk_storage = db.chunks_storage().await?;
    let label_storage = db.label_storage().await?;

    if all {
        println!("🗑️  Purging entire database");

        // Truncate both tables (keeps monodex-meta.json and dataset structure)
        chunk_storage.truncate().await?;
        label_storage.truncate().await?;

        println!("✅ Database purged successfully");
    } else if let Some(catalog_name) = catalog {
        println!("🗑️  Purging catalog: {}", catalog_name);

        // Delete chunks and label metadata for this catalog
        let chunks_deleted = chunk_storage.delete_by_catalog(catalog_name).await?;
        let labels_deleted = label_storage.delete_by_catalog(catalog_name).await?;

        println!(
            "✅ Catalog purged successfully ({} chunks, {} labels deleted)",
            chunks_deleted, labels_deleted
        );
    } else {
        return Err(anyhow::anyhow!(
            "Must specify either --catalog <name> or --all"
        ));
    }

    Ok(())
}
