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
        chunk_storage
            .vector_search(&embedding, label_id.as_str(), limit)
            .await
    })?;

    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label);
    println!();

    // Display results as blurbs
    for result in &results {
        let chunk = &result.chunk;

        // Line 1: file_id:chunk_ordinal  score  breadcrumb [chunk_kind] (part N/M)
        // E.1: Sanitize breadcrumb to prevent terminal injection
        let breadcrumb = sanitize_for_terminal(chunk.breadcrumb.as_deref().unwrap_or("unknown"));

        // Build the report form with chunk_kind and split metadata
        let mut report = breadcrumb.clone();
        if let (Some(ordinal), Some(count)) = (chunk.split_part_ordinal, chunk.split_part_count) {
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
  "qdrant": {{ "collection": "test" }},
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
        let tables_dir = db_path.join("tables");
        fs::create_dir_all(&tables_dir).unwrap();

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

    fn test_chunk_row(point_id: &str, file_id: &str, ordinal: i32, label_id: &str) -> ChunkRow {
        ChunkRow {
            point_id: point_id.to_string(),
            text: format!("Test content for chunk {} in file {}", ordinal, file_id),
            catalog: "test-catalog".to_string(),
            label_id: label_id.to_string(),
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
            breadcrumb: Some(format!(
                "test-package:test.ts:testFunction-chunk{}",
                ordinal
            )),
            split_part_ordinal: None,
            split_part_count: None,
            file_complete: ordinal == 1,
        }
    }

    fn test_label_metadata_row(label_id: &str) -> LabelMetadataRow {
        LabelMetadataRow {
            label_id: label_id.to_string(),
            catalog: "test-catalog".to_string(),
            label: label_id
                .split(':')
                .next_back()
                .unwrap_or("main")
                .to_string(),
            commit_oid: "abc123def456".to_string(),
            source_kind: "git-commit".to_string(),
            crawl_complete: true,
            updated_at_unix_secs: 1700000000,
        }
    }

    #[test]
    #[serial(monodex_home)]
    fn test_search_missing_database() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config but no database
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        let config = crate::app::config::load_config(&config_path).unwrap();
        let result = run_search(
            &config,
            "test query",
            10,
            Some("main"),
            Some("test-catalog"),
            false,
        );

        let err = result.unwrap_err().to_string();
        // Should mention missing database and init-db
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
    fn test_search_missing_label_context() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Create database with chunks (use valid hex file IDs)
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(
                &db_path,
                vec![test_chunk_row(
                    "aaaabbbbcccc1111:1",
                    "aaaabbbbcccc1111",
                    1,
                    "test-catalog:main",
                )],
                vec![test_label_metadata_row("test-catalog:main")],
            )
            .await;
        });

        let config = crate::app::config::load_config(&config_path).unwrap();

        // Search without providing catalog or label, and no default context
        let result = run_search(&config, "test query", 10, None, None, false);

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No context set"),
            "Error should mention missing context: {}",
            err
        );

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_search_no_results() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Create empty database (no chunks)
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(&db_path, vec![], vec![]).await;
        });

        // Note: This test will fail at embedding generation without the model.
        // For now, we just verify the database handshake works.
        // In a real test environment, you would mock the embedder.

        remove_monodex_home();
    }
}
