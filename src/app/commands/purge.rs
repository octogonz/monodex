//! Handler for the `purge` command.
//!
//! Edit here when: Modifying purge behavior (delete catalog or collection).

use crate::app::Config;
use crate::engine::uploader::QdrantUploader;

/// Run purge command (delete all chunks from a catalog or entire collection)
pub fn run_purge(
    config: &Config,
    catalog: Option<&str>,
    all: bool,
    debug: bool,
) -> anyhow::Result<()> {
    let uploader = QdrantUploader::new(
        &config.qdrant.collection,
        config.qdrant.url.as_deref(),
        debug,
        config.qdrant.get_max_upload_bytes(),
    )?;

    if all {
        println!(
            "🗑️  Purging entire collection: {}",
            config.qdrant.collection
        );
        println!("This will delete ALL data from the collection!");

        // Delete all points with empty filter
        let endpoint = format!(
            "{}/collections/{}/points/delete",
            config
                .qdrant
                .url
                .as_deref()
                .unwrap_or("http://localhost:6333"),
            config.qdrant.collection
        );

        let empty_filter = serde_json::json!({"filter": {}});

        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .json(&empty_filter)
            .send()?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to purge collection: HTTP {}",
                response.status()
            ));
        }

        println!("✅ Collection purged successfully");
    } else if let Some(catalog_name) = catalog {
        println!("🗑️  Purging catalog: {}", catalog_name);

        let operation_id = uploader.delete_catalog(catalog_name)?;
        println!(
            "✅ Catalog purged successfully (operation ID: {})",
            operation_id
        );
    } else {
        return Err(anyhow::anyhow!(
            "Must specify either --catalog <name> or --all"
        ));
    }

    Ok(())
}
