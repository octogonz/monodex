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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::schema::{chunks_schema, label_metadata_schema};
    use crate::engine::storage::{
        ChunkRow, Database as StorageDatabase, LabelMetadataRow, META_FILE, MetaFile,
    };
    use crate::paths::clear_tool_home_cache;
    use lancedb::connect;
    use serial_test::serial;
    use std::fs::{self, File};
    use std::io::Write;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Mutex to serialize tests that use MONODEX_HOME environment variable.
    static MONODEX_HOME_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to safely set MONODEX_HOME.
    fn set_monodex_home(path: &std::path::Path) {
        // SAFETY: We hold MONODEX_HOME_MUTEX to ensure no concurrent access.
        unsafe {
            std::env::set_var("MONODEX_HOME", path);
        }
    }

    /// Helper to safely remove MONODEX_HOME.
    fn remove_monodex_home() {
        // SAFETY: We hold MONODEX_HOME_MUTEX to ensure no concurrent access.
        unsafe {
            std::env::remove_var("MONODEX_HOME");
        }
    }

    /// Helper to create a minimal config file.
    fn write_minimal_config(config_path: &std::path::Path) {
        let mut file = File::create(config_path).unwrap();
        writeln!(
            file,
            r#"{{
  "catalogs": {{}}
}}"#
        )
        .unwrap();
    }

    /// Create a test database with chunks and label metadata.
    async fn create_test_db_with_chunks(
        db_path: &std::path::Path,
        chunks: Vec<ChunkRow>,
        labels: Vec<LabelMetadataRow>,
    ) {
        // Create database directory
        fs::create_dir_all(db_path).unwrap();

        // Create LanceDB tables
        let conn = connect(db_path.to_str().unwrap())
            .execute()
            .await
            .expect("Failed to create database");

        conn.create_empty_table("chunks", chunks_schema())
            .execute()
            .await
            .expect("Failed to create chunks table");

        conn.create_empty_table("label_metadata", label_metadata_schema())
            .execute()
            .await
            .expect("Failed to create label_metadata table");

        // Write meta file
        let meta = MetaFile::new();
        let meta_file = File::create(db_path.join(META_FILE)).unwrap();
        serde_json::to_writer_pretty(meta_file, &meta).unwrap();

        // Insert chunks if any
        if !chunks.is_empty() {
            let db = StorageDatabase::open(db_path).await.unwrap();
            let chunk_storage = db.chunks_storage().await.unwrap();
            chunk_storage.upsert(&chunks).await.unwrap();
        }

        // Insert labels if any
        if !labels.is_empty() {
            let db = StorageDatabase::open(db_path).await.unwrap();
            let label_storage = db.label_storage().await.unwrap();
            for label in labels {
                label_storage.upsert(&label).await.unwrap();
            }
        }
    }

    fn test_chunk_row(
        point_id: &str,
        file_id: &str,
        ordinal: i32,
        catalog: &str,
        label_id: &str,
    ) -> ChunkRow {
        ChunkRow {
            point_id: point_id.to_string(),
            text: format!("Test content for chunk {}", ordinal),
            catalog: catalog.to_string(),
            active_label_ids: vec![label_id.to_string()],
            embedder_id: "test-embedder:v1".to_string(),
            chunker_id: "test-chunker:v1".to_string(),
            blob_id: "abc123".to_string(),
            content_hash: "def456".to_string(),
            file_id: file_id.to_string(),
            relative_path: "src/test.ts".to_string(),
            package_name: "test-package".to_string(),
            source_uri: "/path/to/test.ts".to_string(),
            chunk_ordinal: ordinal,
            chunk_count: 3,
            start_line: 1,
            end_line: 50,
            symbol_name: Some("testFunction".to_string()),
            chunk_type: "function".to_string(),
            chunk_kind: "content".to_string(),
            breadcrumb: Some("test-package:test.ts:testFunction".to_string()),
            split_part_ordinal: None,
            split_part_count: None,
            file_complete: ordinal == 1,
        }
    }

    fn test_label_metadata_row(catalog: &str, label: &str) -> LabelMetadataRow {
        LabelMetadataRow {
            label_id: format!("{}:{}", catalog, label),
            catalog: catalog.to_string(),
            label: label.to_string(),
            commit_oid: "abc123def456".to_string(),
            source_kind: "git-commit".to_string(),
            crawl_complete: true,
            updated_at_unix_secs: 1700000000,
        }
    }

    #[test]
    #[serial(monodex_home)]
    fn test_purge_missing_database() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config but no database
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        let config = crate::app::config::load_config(&config_path).unwrap();
        let result = run_purge(&config, Some("test-catalog"), false, false);

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No monodex database"),
            "Error should mention missing database: {}",
            err
        );
        assert!(
            err.contains("init-db"),
            "Error should mention init-db: {}",
            err
        );

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_purge_neither_catalog_nor_all() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Create database
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(&db_path, vec![], vec![]).await;
        });

        let config = crate::app::config::load_config(&config_path).unwrap();
        let result = run_purge(&config, None, false, false);

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Must specify either --catalog"),
            "Error should mention missing options: {}",
            err
        );

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_purge_all_truncates_tables() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Create database with chunks and labels
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(
                &db_path,
                vec![
                    test_chunk_row("file1:1", "file1", 1, "catalog1", "catalog1:main"),
                    test_chunk_row("file2:1", "file2", 1, "catalog2", "catalog2:main"),
                ],
                vec![
                    test_label_metadata_row("catalog1", "main"),
                    test_label_metadata_row("catalog2", "main"),
                ],
            )
            .await;
        });

        let config = crate::app::config::load_config(&config_path).unwrap();
        let result = run_purge(&config, None, true, false);

        assert!(
            result.is_ok(),
            "purge --all should succeed: {:?}",
            result.err()
        );

        // Verify tables are empty
        rt.block_on(async {
            let db = StorageDatabase::open(&db_path).await.unwrap();
            let chunk_storage = db.chunks_storage().await.unwrap();
            let label_storage = db.label_storage().await.unwrap();

            let chunk_count = chunk_storage.table().count_rows(None).await.unwrap();
            let label_count = label_storage.table().count_rows(None).await.unwrap();

            assert_eq!(chunk_count, 0, "Chunks table should be empty");
            assert_eq!(label_count, 0, "Labels table should be empty");
        });

        // Verify meta file still exists
        assert!(
            db_path.join(META_FILE).exists(),
            "Meta file should still exist"
        );

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_purge_catalog_deletes_only_that_catalog() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Create database with chunks from multiple catalogs
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(
                &db_path,
                vec![
                    test_chunk_row("file1:1", "file1", 1, "catalog1", "catalog1:main"),
                    test_chunk_row("file2:1", "file2", 1, "catalog1", "catalog1:main"),
                    test_chunk_row("file3:1", "file3", 1, "catalog2", "catalog2:main"),
                ],
                vec![
                    test_label_metadata_row("catalog1", "main"),
                    test_label_metadata_row("catalog2", "main"),
                ],
            )
            .await;
        });

        let config = crate::app::config::load_config(&config_path).unwrap();
        let result = run_purge(&config, Some("catalog1"), false, false);

        assert!(
            result.is_ok(),
            "purge catalog1 should succeed: {:?}",
            result.err()
        );

        // Verify only catalog1 was deleted
        rt.block_on(async {
            let db = StorageDatabase::open(&db_path).await.unwrap();
            let chunk_storage = db.chunks_storage().await.unwrap();
            let label_storage = db.label_storage().await.unwrap();

            let chunk_count = chunk_storage.table().count_rows(None).await.unwrap();
            let label_count = label_storage.table().count_rows(None).await.unwrap();

            assert_eq!(chunk_count, 1, "Only catalog2 chunks should remain");
            assert_eq!(label_count, 1, "Only catalog2 label should remain");
        });

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_purge_nonexistent_catalog_succeeds() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Create database with chunks
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(
                &db_path,
                vec![test_chunk_row(
                    "file1:1",
                    "file1",
                    1,
                    "catalog1",
                    "catalog1:main",
                )],
                vec![test_label_metadata_row("catalog1", "main")],
            )
            .await;
        });

        let config = crate::app::config::load_config(&config_path).unwrap();
        let result = run_purge(&config, Some("nonexistent-catalog"), false, false);

        // Should succeed (deletes 0 rows)
        assert!(
            result.is_ok(),
            "purge nonexistent catalog should succeed: {:?}",
            result.err()
        );

        // Verify original data is still there
        rt.block_on(async {
            let db = StorageDatabase::open(&db_path).await.unwrap();
            let chunk_storage = db.chunks_storage().await.unwrap();

            let chunk_count = chunk_storage.table().count_rows(None).await.unwrap();
            assert_eq!(chunk_count, 1, "Original chunks should still exist");
        });

        remove_monodex_home();
    }
}
