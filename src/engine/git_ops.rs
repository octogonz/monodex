//! Git operations for commit-based and working directory crawling
//!
//! This module provides functions to read file content and metadata
//! from Git commits without touching the working tree, as well as
//! enumerating files from the working directory for indexing uncommitted changes.

use anyhow::{Result, anyhow};
use gix::ObjectId;
use gix::objs::TreeRefIter;
use gix::traverse::tree::Recorder;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

/// A file entry from a Git commit tree
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Relative path from repo root
    pub relative_path: String,
    /// Git blob SHA (object ID)
    pub blob_id: String,
}

/// Resolve a commit reference to its full SHA
///
/// Supports HEAD, branch names, tags, and SHA prefixes.
/// Returns the full 40-character hex SHA.
pub fn resolve_commit_oid(repo_path: &Path, commit: &str) -> Result<String> {
    // Open the repository
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    // Resolve the commit reference
    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    Ok(commit_id.to_hex().to_string())
}

/// Package index for resolving package names from file paths
///
/// Maps directory paths to package names extracted from package.json files.
/// Keys are repo-relative directory paths (e.g., "libraries/node-core-library").
pub struct PackageIndex {
    /// Map from directory path to package name
    /// Key: relative directory path (e.g., "libraries/node-core-library")
    /// Value: package name (e.g., "@rushstack/node-core-library")
    package_name_by_dir: HashMap<String, String>,
}

impl PackageIndex {
    /// Create a new empty package index
    pub fn new() -> Self {
        Self {
            package_name_by_dir: HashMap::new(),
        }
    }

    /// Find the package name for a given file path
    ///
    /// Walks ancestor directories to find the nearest package.json.
    /// Returns None if no package is found.
    pub fn find_package_name(&self, relative_path: &str) -> Option<&str> {
        let path = Path::new(relative_path);
        let mut current = path.parent().unwrap_or(path);

        // Walk upwards checking each directory
        loop {
            let dir_str = current.to_string_lossy();
            let dir_key = dir_str.replace('\\', "/");

            if let Some(name) = self.package_name_by_dir.get(&dir_key) {
                return Some(name);
            }

            // Check if we're at root (empty string key)
            if current == Path::new("") || current == Path::new(".") {
                // Check root package.json
                if let Some(name) = self.package_name_by_dir.get("") {
                    return Some(name);
                }
                break;
            }

            // Go to parent
            match current.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }

        None
    }
}

impl Default for PackageIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Enumerate all files in a commit tree
///
/// Returns a list of file entries with their paths and blob IDs.
/// Filters out non-blob entries (submodules, symlinks, etc.)
pub fn enumerate_commit_tree(repo_path: &Path, commit: &str) -> Result<Vec<FileEntry>> {
    // Open the repository
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    // Resolve the commit reference (supports HEAD, branch names, SHA prefixes)
    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    // Get the tree ID from the commit
    let commit_obj = repo
        .find_object(commit_id)
        .map_err(|e| anyhow!("Failed to find commit object: {}", e))?;

    let tree_id: ObjectId = {
        let commit = commit_obj
            .try_into_commit()
            .map_err(|_| anyhow!("'{}' is not a commit", commit))?;
        commit
            .tree_id()
            .map_err(|e| anyhow!("Failed to get tree ID: {}", e))?
            .detach()
    };
    // commit is now dropped, releasing borrow on repo

    // Get the tree data
    let tree_data = {
        let tree_obj = repo
            .find_object(tree_id)
            .map_err(|e| anyhow!("Failed to find tree object: {}", e))?;
        tree_obj.data.clone()
    };
    // tree_obj is now dropped, releasing borrow on repo

    // Use a Recorder to collect all entries with their full paths
    let mut recorder = Recorder::default();
    gix::traverse::tree::breadthfirst(
        TreeRefIter::from_bytes(&tree_data),
        &mut gix::traverse::tree::breadthfirst::State::default(),
        repo.objects,
        &mut recorder,
    )
    .map_err(|e| anyhow!("Failed to traverse tree: {}", e))?;

    // Filter to only include blob entries (regular files)
    let entries: Vec<FileEntry> = recorder
        .records
        .into_iter()
        .filter(|entry| entry.mode.is_blob())
        .map(|entry| FileEntry {
            relative_path: entry.filepath.to_string(),
            blob_id: entry.oid.to_hex().to_string(),
        })
        .collect();

    Ok(entries)
}

/// Read the content of a Git blob
///
/// Returns the raw bytes of the blob content.
pub fn read_blob_content(repo_path: &Path, blob_id: &str) -> Result<Vec<u8>> {
    // Open the repository
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    // Parse the blob ID from hex string
    let object_id = ObjectId::from_hex(blob_id.as_bytes())
        .map_err(|e| anyhow!("Invalid blob ID '{}': {}", blob_id, e))?;

    // Find the blob object
    let blob = repo
        .find_object(object_id)
        .map_err(|e| anyhow!("Failed to find blob '{}': {}", blob_id, e))?
        .try_into_blob()
        .map_err(|_| anyhow!("Object '{}' is not a blob", blob_id))?;

    Ok(blob.data.to_vec())
}

/// Build a package index for a Git commit
///
/// Enumerates all package.json files in the commit, reads their content,
/// and builds a mapping from directory paths to package names.
pub fn build_package_index_for_commit(repo_path: &Path, commit: &str) -> Result<PackageIndex> {
    // Open the repository
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    // Resolve the commit reference
    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    // Get the commit object
    let commit_obj = repo
        .find_object(commit_id)
        .map_err(|e| anyhow!("Failed to find commit object: {}", e))?;

    let tree_id: ObjectId = {
        let commit = commit_obj
            .try_into_commit()
            .map_err(|_| anyhow!("'{}' is not a commit", commit))?;
        commit
            .tree_id()
            .map_err(|e| anyhow!("Failed to get tree ID: {}", e))?
            .detach()
    };

    // Get the tree data
    let tree_data = {
        let tree_obj = repo
            .find_object(tree_id)
            .map_err(|e| anyhow!("Failed to find tree object: {}", e))?;
        tree_obj.data.clone()
    };

    // Use a Recorder to collect all entries
    let mut recorder = Recorder::default();
    gix::traverse::tree::breadthfirst(
        TreeRefIter::from_bytes(&tree_data),
        &mut gix::traverse::tree::breadthfirst::State::default(),
        repo.objects.clone(),
        &mut recorder,
    )
    .map_err(|e| anyhow!("Failed to traverse tree: {}", e))?;

    // Collect package.json entries (dir_path, blob_id)
    let package_json_entries: Vec<(String, ObjectId)> = recorder
        .records
        .iter()
        .filter(|entry| entry.mode.is_blob())
        .filter_map(|entry| {
            let filepath_bytes: &[u8] = entry.filepath.as_ref();
            let filename = filepath_bytes
                .rsplit(|b| *b == b'/')
                .next()
                .unwrap_or_default();

            if filename == b"package.json" {
                let dir_path = filepath_bytes
                    .rsplit(|b| *b == b'/')
                    .nth(1)
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .unwrap_or_default();
                Some((dir_path, entry.oid))
            } else {
                None
            }
        })
        .collect();

    // Build package index by reading package.json blobs
    let mut index = PackageIndex::new();

    for (dir_path, blob_id) in package_json_entries {
        // Read the blob content
        if let Ok(obj) = repo.find_object(blob_id) {
            if let Ok(blob) = obj.try_into_blob() {
                // Parse the "name" field from the blob content
                if let Some(name) = extract_package_name_from_bytes(&blob.data) {
                    index.package_name_by_dir.insert(dir_path, name);
                }
            }
        }
    }

    Ok(index)
}

/// Extract the "name" field from package.json content
///
/// Simple string search for "name": "value" pattern.
/// No need for a full JSON parser since we only need the name field.
fn extract_package_name_from_bytes(content: &[u8]) -> Option<String> {
    let content_str = std::str::from_utf8(content).ok()?;

    // Find "name": "value" in JSON
    let name_key = "\"name\"";
    let key_pos = content_str.find(name_key)?;

    // Find the colon after "name"
    let after_key = &content_str[key_pos + name_key.len()..];
    let colon_pos = after_key.find(':')?;

    // Find the opening quote of the value
    let after_colon = &after_key[colon_pos + 1..];
    let first_quote = after_colon.find('"')?;

    // Find the closing quote
    let value_start = first_quote + 1;
    let after_first_quote = &after_colon[value_start..];
    let end_quote = after_first_quote.find('"')?;

    Some(after_first_quote[..end_quote].to_string())
}

// ========================================
// Working Directory Operations
// ========================================

/// A file entry from the working directory for indexing uncommitted changes.
///
/// Unlike commit-based FileEntry, this uses a content hash instead of blob_id
/// because the content may not be in Git yet.
#[derive(Debug, Clone)]
pub struct WorkingDirEntry {
    /// Relative path from repo root
    pub relative_path: String,
    /// Content hash (SHA256 of file content)
    pub content_hash: String,
}

/// Enumerate files from the working directory.
///
/// Walks the filesystem, respecting .gitignore patterns and applying
/// exclusion rules. Returns entries with content hashes for identity.
///
/// # Arguments
/// * `repo_path` - Root path of the repository
/// * `should_skip` - Function to determine if a path should be skipped
///
/// # Returns
/// Vector of WorkingDirEntry with relative paths and content hashes
pub fn enumerate_working_directory<F>(
    repo_path: &Path,
    should_skip: F,
) -> Result<Vec<WorkingDirEntry>>
where
    F: Fn(&str) -> bool,
{
    use std::fs;

    let mut entries: Vec<WorkingDirEntry> = Vec::new();

    // Use walkdir to traverse the filesystem
    for entry in walkdir::WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip hidden directories and common exclusions
            let path = e.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();

            // Skip hidden directories (except .git is handled separately)
            if name.starts_with('.') && name != ".git" {
                return false;
            }

            // Skip common directories that should never be indexed
            if matches!(
                name.as_ref(),
                "node_modules" | "target" | "dist" | "build" | ".cache" | "temp"
            ) {
                return false;
            }

            true
        })
    {
        let entry = entry.map_err(|e| anyhow!("Failed to read directory entry: {}", e))?;

        let path = entry.path();

        // Only process files
        if !path.is_file() {
            continue;
        }

        // Get relative path from repo root
        let relative_path = path
            .strip_prefix(repo_path)
            .map_err(|e| anyhow!("Failed to strip prefix: {}", e))?
            .to_string_lossy()
            .replace('\\', "/");

        // Apply should_skip filter
        if should_skip(&relative_path) {
            continue;
        }

        // Read file content and compute hash
        let content = match fs::read(path) {
            Ok(c) => c,
            Err(e) => {
                // Skip files we can't read (permissions, etc.)
                eprintln!("  ⚠️  Skipping {} (can't read: {})", relative_path, e);
                continue;
            }
        };

        // Compute content hash
        let content_hash = compute_content_hash(&content);

        entries.push(WorkingDirEntry {
            relative_path,
            content_hash,
        });
    }

    Ok(entries)
}

/// Compute SHA256 hash of content.
///
/// Used as blob_id substitute for working directory files that aren't in Git yet.
fn compute_content_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let result = hasher.finalize();
    format!("sha256:{:x}", result)
}

/// Build a package index from the working directory.
///
/// Walks the filesystem to find package.json files and extracts their names.
/// This is the working directory equivalent of build_package_index_for_commit.
///
/// # Arguments
/// * `repo_path` - Root path of the repository
///
/// # Returns
/// PackageIndex mapping directory paths to package names
pub fn build_package_index_for_working_dir(repo_path: &Path) -> Result<PackageIndex> {
    let mut index = PackageIndex::new();

    // Walk the filesystem looking for package.json files
    for entry in walkdir::WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let path = e.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();

            // Skip hidden directories and common exclusions
            if name.starts_with('.') && name != ".git" {
                return false;
            }

            if matches!(
                name.as_ref(),
                "node_modules" | "target" | "dist" | "build" | ".cache" | "temp"
            ) {
                return false;
            }

            true
        })
    {
        let entry = entry.map_err(|e| anyhow!("Failed to read directory entry: {}", e))?;
        let path = entry.path();

        // Only process package.json files
        if !path.is_file() {
            continue;
        }

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        if file_name != "package.json" {
            continue;
        }

        // Get relative directory path from repo root
        let dir_path = path
            .parent()
            .and_then(|p| p.strip_prefix(repo_path).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();

        // Read and parse package.json
        if let Ok(content) = std::fs::read(path) {
            if let Some(name) = extract_package_name_from_bytes(&content) {
                index.package_name_by_dir.insert(dir_path, name);
            }
        }
    }

    Ok(index)
}

/// Read file content from the working directory.
///
/// Simple wrapper around fs::read that returns the content as bytes.
pub fn read_working_file_content(repo_path: &Path, relative_path: &str) -> Result<Vec<u8>> {
    let full_path = repo_path.join(relative_path);
    std::fs::read(&full_path).map_err(|e| anyhow!("Failed to read file {}: {}", relative_path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_enumerate_commit_tree_current_repo() {
        // Get the current repo path (monodex repo itself)
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // Enumerate files in HEAD
        let entries = enumerate_commit_tree(&repo_path, "HEAD").expect("Failed to enumerate");

        // Should have at least some files
        assert!(!entries.is_empty(), "Should have found some files");

        // Check that some expected files exist
        let readme = entries.iter().find(|e| e.relative_path == "README.md");
        assert!(readme.is_some(), "Should have found README.md");

        let cargo = entries.iter().find(|e| e.relative_path == "Cargo.toml");
        assert!(cargo.is_some(), "Should have found Cargo.toml");

        // Print some stats
        println!("Found {} files in HEAD", entries.len());
        println!("First 10 files:");
        for entry in entries.iter().take(10) {
            println!("  {} (blob: {})", entry.relative_path, &entry.blob_id[..8]);
        }
    }

    #[test]
    fn test_read_blob_content_current_repo() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // First enumerate to get a blob ID
        let entries = enumerate_commit_tree(&repo_path, "HEAD").expect("Failed to enumerate");
        let readme = entries
            .iter()
            .find(|e| e.relative_path == "README.md")
            .expect("README.md not found");

        // Read the blob content
        let content = read_blob_content(&repo_path, &readme.blob_id).expect("Failed to read blob");

        // Verify it contains expected content
        let content_str = String::from_utf8_lossy(&content);
        assert!(
            content_str.contains("Monodex"),
            "README should contain 'Monodex'"
        );

        println!(
            "README.md content (first 200 chars):\n{}",
            &content_str[..200.min(content_str.len())]
        );
    }

    #[test]
    fn test_build_package_index() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // Build package index for HEAD
        let index =
            build_package_index_for_commit(&repo_path, "HEAD").expect("Failed to build index");

        // This repo has a Cargo.toml at root, no package.json
        // So we shouldn't find any packages
        println!(
            "Package index has {} entries",
            index.package_name_by_dir.len()
        );

        // Print any packages found
        for (dir, name) in &index.package_name_by_dir {
            println!("  {} -> {}", dir, name);
        }

        // Test lookup with a hypothetical file
        if !index.package_name_by_dir.is_empty() {
            let result = index.find_package_name("src/main.rs");
            println!("Package for src/main.rs: {:?}", result);
        }
    }

    #[test]
    fn test_extract_package_name_from_bytes() {
        let json = br#"{"name": "@scope/package-name", "version": "1.0.0"}"#;
        let name = extract_package_name_from_bytes(json);
        assert_eq!(name, Some("@scope/package-name".to_string()));

        let json2 = br#"{
  "name": "simple-package",
  "version": "2.0.0"
}"#;
        let name2 = extract_package_name_from_bytes(json2);
        assert_eq!(name2, Some("simple-package".to_string()));
    }

    #[test]
    fn test_enumerate_working_directory() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // Enumerate working directory files
        let entries = enumerate_working_directory(&repo_path, |_path| false)
            .expect("Failed to enumerate working directory");

        // Should have at least some files
        assert!(!entries.is_empty(), "Should have found some files");

        // Check that some expected files exist
        let readme = entries.iter().find(|e| e.relative_path == "README.md");
        assert!(readme.is_some(), "Should have found README.md");

        // Content hashes should be non-empty and start with "sha256:"
        for entry in entries.iter().take(5) {
            assert!(
                entry.content_hash.starts_with("sha256:"),
                "Content hash should start with sha256:"
            );
            println!("  {} ({})", entry.relative_path, &entry.content_hash[..16]);
        }
    }

    #[test]
    fn test_compute_content_hash() {
        let content = b"Hello, world!";
        let hash = compute_content_hash(content);

        // SHA256 hashes should be 64 hex chars + "sha256:" prefix
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), 64 + 7); // 64 hex chars + "sha256:" prefix

        // Same content should produce same hash
        let hash2 = compute_content_hash(content);
        assert_eq!(hash, hash2);

        // Different content should produce different hash
        let hash3 = compute_content_hash(b"Different content");
        assert_ne!(hash, hash3);
    }
}
