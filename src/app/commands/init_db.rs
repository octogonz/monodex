//! Database initialization command.
//!
//! Purpose: Create a new monodex database directory with LanceDB tables.
//! Edit here when: Changing init-db behavior, error messages, or initialization logic.
//! Do not edit here for: Database open logic (see engine/storage/database.rs),
//!   schema definitions (see engine/schema.rs), config loading (see app/config.rs).

use anyhow::{Result, anyhow, bail};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use crate::app::config::{load_config, resolve_database_path};
use crate::engine::schema::{
    CHUNKS_TABLE, LABEL_METADATA_TABLE, chunks_schema, label_metadata_schema,
};
use crate::engine::storage::{Database, META_FILE, MetaFile};
use crate::paths;

// ============================================================================
// Error message constants
// These are load-bearing user-facing strings. Tests assert exact matches.
// ============================================================================

/// Error when config file is missing.
pub const ERR_CONFIG_MISSING: &str = "No config found at {}. Create one before running init-db.";

/// Error when database parent directory does not exist (non-default-db case).
pub const ERR_PARENT_MISSING: &str =
    "Cannot create database at {}: parent directory does not exist.";

/// Error when path exists but is not a monodex database.
pub const ERR_NOT_MONODEX_DB: &str = "Path {} exists but is not a monodex database.";

/// Error when path is partially initialized or corrupted.
pub const ERR_PARTIAL_STATE: &str = "Path {} appears to be a partially-initialized or corrupted monodex database. Manual cleanup required.";

/// Log message when database is already initialized.
pub const LOG_ALREADY_INITIALIZED: &str =
    "Database at {} is already initialized (monodex_schema_version {}); skipping.";

// ============================================================================
// Error message helpers
// Use these instead of str::replace to avoid fragility.
// ============================================================================

/// Format the "config missing" error with the resolved config path.
fn err_config_missing(path: &Path) -> String {
    format!(
        "No config found at {}. Create one before running init-db.",
        path.display()
    )
}

/// Format the "parent missing" error with the database path.
fn err_parent_missing(db_path: &Path) -> String {
    format!(
        "Cannot create database at {}: parent directory does not exist.",
        db_path.display()
    )
}

/// Format the "not a monodex database" error with the database path.
fn err_not_monodex_db(db_path: &Path) -> String {
    format!(
        "Path {} exists but is not a monodex database.",
        db_path.display()
    )
}

/// Format the "partial state" error with the database path.
fn err_partial_state(db_path: &Path) -> String {
    format!(
        "Path {} appears to be a partially-initialized or corrupted monodex database. Manual cleanup required.",
        db_path.display()
    )
}

/// Format the "already initialized" log message with the database path and schema version.
fn log_already_initialized(db_path: &Path, schema_version: u32) -> String {
    format!(
        "Database at {} is already initialized (monodex_schema_version {}); skipping.",
        db_path.display(),
        schema_version
    )
}

// ============================================================================
// Command entry point
// ============================================================================

/// Run the init-db command.
///
/// Creates a new monodex database at the configured path, or verifies an existing
/// database is valid. The database contains LanceDB tables for chunks and label metadata.
pub fn run_init_db() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow!("Failed to create tokio runtime: {}", e))?;
    rt.block_on(init_db_inner())
}

async fn init_db_inner() -> Result<()> {
    // Load config to get database path
    let config_path = paths::config_path()?;

    if !config_path.exists() {
        bail!(err_config_missing(&config_path));
    }

    // Load config and resolve database path
    let config = load_config(&config_path)?;
    let db_path = resolve_database_path(Some(&config))?;

    // Check if already initialized
    if let Some(meta) = check_existing_database(&db_path)? {
        println!(
            "{}",
            log_already_initialized(&db_path, meta.monodex_schema_version)
        );
        return Ok(());
    }

    // Validate parent directory exists (with exception for default-db)
    validate_parent_directory(&db_path)?;

    // Create the database
    create_database(&db_path).await?;

    println!("Created monodex database at {}", db_path.display());
    Ok(())
}

/// Check if a database already exists at the given path.
/// Returns Some(MetaFile) if valid database exists, None if path doesn't exist.
/// Returns error if path exists but is not a valid database.
fn check_existing_database(db_path: &Path) -> Result<Option<MetaFile>> {
    if !db_path.exists() {
        return Ok(None);
    }

    // Check if it's a valid monodex database
    let meta_path = db_path.join(META_FILE);

    if meta_path.exists() {
        // Try to load meta file
        match Database::load_meta(&meta_path) {
            Ok(meta) => Ok(Some(meta)),
            Err(_) => {
                // Corrupted meta file
                bail!(err_partial_state(db_path));
            }
        }
    } else {
        // Check if directory is empty
        let is_empty = db_path
            .read_dir()
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);

        if is_empty {
            // Empty directory, treat as non-existent
            Ok(None)
        } else {
            // Non-empty without meta file
            bail!(err_not_monodex_db(db_path));
        }
    }
}

/// Validate that the parent directory exists, with exception for default-db.
fn validate_parent_directory(db_path: &Path) -> Result<()> {
    // Special case: if the path is under tool_home, we can create tool_home itself
    let tool_home = paths::tool_home()?;
    let is_under_tool_home = db_path.starts_with(&tool_home);

    if let Some(parent) = db_path.parent()
        && !parent.exists()
    {
        // If parent is tool_home itself, we can create it
        if is_under_tool_home && parent == tool_home {
            // tool_home will be created by create_database
            return Ok(());
        }
        bail!(err_parent_missing(db_path));
    }

    Ok(())
}

/// Create the database directory and initialize LanceDB tables.
async fn create_database(db_path: &Path) -> Result<()> {
    // Create the database root directory
    fs::create_dir_all(db_path)?;

    // Acquire exclusive lock
    let lock_path = db_path.join(".monodex.lock");
    let lock_file = File::create(&lock_path)?;

    // Use fs4 for exclusive lock
    fs4::fs_std::FileExt::lock_exclusive(&lock_file)?;

    // Double-check after acquiring lock (another process may have created it)
    let meta_path = db_path.join(META_FILE);
    if meta_path.exists() {
        // Another process created it, we're done
        fs4::fs_std::FileExt::unlock(&lock_file)?;
        let _ = fs::remove_file(&lock_path);
        return Ok(());
    }

    // Create tables directory (LanceDB expects this)
    let tables_dir = db_path.join("tables");
    fs::create_dir_all(&tables_dir)?;

    // Open LanceDB connection
    let conn = lancedb::connect(db_path.to_str().unwrap())
        .execute()
        .await
        .map_err(|e| anyhow!("Failed to create LanceDB database: {}", e))?;

    // Create chunks table
    conn.create_empty_table(CHUNKS_TABLE, chunks_schema())
        .execute()
        .await
        .map_err(|e| anyhow!("Failed to create chunks table: {}", e))?;

    // Create label_metadata table
    conn.create_empty_table(LABEL_METADATA_TABLE, label_metadata_schema())
        .execute()
        .await
        .map_err(|e| anyhow!("Failed to create label_metadata table: {}", e))?;

    // Write meta file
    let meta = MetaFile::new();
    let meta_file = File::create(&meta_path)?;
    let writer = BufWriter::new(meta_file);
    serde_json::to_writer_pretty(writer, &meta)
        .map_err(|e| anyhow!("Failed to write {}: {}", meta_path.display(), e))?;

    // Sync the directory to ensure durability
    #[cfg(unix)]
    {
        let dir_file = File::open(db_path)?;
        dir_file.sync_all()?;
    }

    // Release lock
    fs4::fs_std::FileExt::unlock(&lock_file)?;

    // Clean up lock file
    let _ = fs::remove_file(&lock_path);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::clear_tool_home_cache;
    use serial_test::serial;
    use std::io::Write;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Mutex to serialize tests that use MONODEX_HOME environment variable.
    /// MONODEX_HOME is process-global, so tests that modify it cannot run in parallel.
    static MONODEX_HOME_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to create a minimal config file in the given directory.
    fn write_minimal_config(config_path: &Path) {
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

    /// Helper to create a config file with a custom database path.
    fn write_config_with_db_path(config_path: &Path, db_path: &str) {
        let mut file = File::create(config_path).unwrap();
        writeln!(
            file,
            r#"{{
  "qdrant": {{ "collection": "test" }},
  "catalogs": {{}},
  "database": {{ "path": "{}" }}
}}"#,
            db_path
        )
        .unwrap();
    }

    /// Helper to safely set MONODEX_HOME (unsafe required in Rust 2024 edition).
    fn set_monodex_home(path: &Path) {
        // SAFETY: We hold MONODEX_HOME_MUTEX to ensure no concurrent access.
        unsafe {
            std::env::set_var("MONODEX_HOME", path);
        }
    }

    /// Helper to safely remove MONODEX_HOME (unsafe required in Rust 2024 edition).
    fn remove_monodex_home() {
        // SAFETY: We hold MONODEX_HOME_MUTEX to ensure no concurrent access.
        unsafe {
            std::env::remove_var("MONODEX_HOME");
        }
    }

    #[test]
    fn test_error_messages_are_const() {
        // This test exists to document that these strings are load-bearing
        assert!(!ERR_CONFIG_MISSING.is_empty());
        assert!(!ERR_PARENT_MISSING.is_empty());
        assert!(!ERR_NOT_MONODEX_DB.is_empty());
        assert!(!ERR_PARTIAL_STATE.is_empty());
        assert!(!LOG_ALREADY_INITIALIZED.is_empty());
    }

    #[test]
    #[serial(monodex_home)]
    fn test_happy_path_creates_database() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        // Set MONODEX_HOME to temp directory
        set_monodex_home(temp_dir.path());

        // Create minimal config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Run init-db
        let result = run_init_db();

        // Should succeed
        assert!(result.is_ok(), "init-db should succeed: {:?}", result.err());

        // Verify structure
        let db_path = temp_dir.path().join("default-db");
        assert!(db_path.exists(), "Database directory should exist");
        assert!(
            db_path.join(META_FILE).exists(),
            "monodex-meta.json should exist"
        );
        assert!(db_path.join("tables").exists(), "tables/ should exist");

        // Cleanup env
        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_idempotent_second_run_succeeds() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // First run
        let result1 = run_init_db();
        assert!(result1.is_ok(), "First init-db should succeed");

        // Second run
        clear_tool_home_cache(); // Clear cache for second run
        let result2 = run_init_db();
        assert!(result2.is_ok(), "Second init-db should succeed");

        // Verify database still valid
        let db_path = temp_dir.path().join("default-db");
        assert!(db_path.join(META_FILE).exists());

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_missing_config_file() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Do NOT create config file
        let config_path = temp_dir.path().join("config.json");

        let result = run_init_db();
        let err = result.unwrap_err();

        // Exact match on error message
        assert_eq!(err.to_string(), err_config_missing(&config_path));

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_parent_missing_non_default_db() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Use an absolute path whose parent definitely doesn't exist
        let db_path_str = "/nonexistent-xyz-12345/db";
        let config_path = temp_dir.path().join("config.json");
        write_config_with_db_path(&config_path, db_path_str);

        let result = run_init_db();
        let err = result.unwrap_err();

        // Exact match on error message
        let expected_db_path = std::path::PathBuf::from(db_path_str);
        assert_eq!(err.to_string(), err_parent_missing(&expected_db_path));

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_path_exists_but_not_monodex_database() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create a directory with a stray file (not a monodex database)
        let db_path = temp_dir.path().join("my-db");
        fs::create_dir_all(&db_path).unwrap();
        File::create(db_path.join("stray-file.txt"))
            .unwrap()
            .write_all(b"not a monodex database")
            .unwrap();

        let config_path = temp_dir.path().join("config.json");
        write_config_with_db_path(&config_path, db_path.to_str().unwrap());

        let result = run_init_db();
        let err = result.unwrap_err();

        // Exact match on error message
        assert_eq!(err.to_string(), err_not_monodex_db(&db_path));

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_corrupt_meta_file() {
        let _guard = MONODEX_HOME_MUTEX.lock().unwrap();
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // First, create a valid database
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        let result = run_init_db();
        assert!(result.is_ok(), "Initial init-db should succeed");

        // Corrupt the meta file
        let db_path = temp_dir.path().join("default-db");
        let meta_path = db_path.join(META_FILE);
        let mut file = File::create(&meta_path).unwrap();
        file.write_all(b"this is not valid json").unwrap();

        // Try to run init-db again
        clear_tool_home_cache(); // Clear cache for second run
        let result = run_init_db();
        let err = result.unwrap_err();

        // Exact match on error message
        assert_eq!(err.to_string(), err_partial_state(&db_path));

        remove_monodex_home();
    }
}
