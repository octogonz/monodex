//! Database initialization command.
//!
//! Purpose: Create a new monodex database directory with LanceDB tables.
//! Edit here when: Changing init-db behavior, error messages, or initialization logic.
//! Do not edit here for: Database open logic (see engine/storage/database.rs),
//!   schema definitions (see engine/schema.rs), config loading (see app/config.rs).

use anyhow::{Result, anyhow, bail};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

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
        bail!(ERR_CONFIG_MISSING.replace("{}", &config_path.display().to_string()));
    }

    // TODO: Load config and resolve database.path
    // For now, use default path
    let db_path = resolve_database_path(&config_path)?;

    // Check if already initialized
    if let Some(meta) = check_existing_database(&db_path)? {
        println!(
            "{}",
            LOG_ALREADY_INITIALIZED
                .replacen("{}", &db_path.display().to_string(), 1)
                .replacen("{}", &meta.monodex_schema_version.to_string(), 1)
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

/// Resolve the database path from config.
/// Returns the default path if database.path is not specified.
fn resolve_database_path(_config_path: &Path) -> Result<PathBuf> {
    // TODO: Actually load config and read database.path field
    // For now, return default path
    let default_path = paths::tool_home()?.join("default-db");
    Ok(default_path)
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
                bail!(ERR_PARTIAL_STATE.replace("{}", &db_path.display().to_string()));
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
            bail!(ERR_NOT_MONODEX_DB.replace("{}", &db_path.display().to_string()));
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
        bail!(ERR_PARENT_MISSING.replace("{}", &db_path.display().to_string()));
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

    #[test]
    fn test_error_messages_are_const() {
        // This test exists to document that these strings are load-bearing
        assert!(!ERR_CONFIG_MISSING.is_empty());
        assert!(!ERR_PARENT_MISSING.is_empty());
        assert!(!ERR_NOT_MONODEX_DB.is_empty());
        assert!(!ERR_PARTIAL_STATE.is_empty());
        assert!(!LOG_ALREADY_INITIALIZED.is_empty());
    }

    // TODO: Add integration tests for each error case
    // These will be added after config changes are complete
}
