//! Git operations for commit-based crawling
//!
//! This module provides functions to read file content and metadata
//! from Git commits without touching the working tree.

use anyhow::{anyhow, Result};
use gix::objs::TreeRefIter;
use gix::traverse::tree::Recorder;
use gix::ObjectId;
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
    let object_id =
        ObjectId::from_hex(blob_id.as_bytes()).map_err(|e| anyhow!("Invalid blob ID '{}': {}", blob_id, e))?;

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
            let filename = filepath_bytes.rsplit(|b| *b == b'/').next().unwrap_or_default();

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
        println!("Package index has {} entries", index.package_name_by_dir.len());

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
}
