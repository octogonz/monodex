//! Git operations for commit-based and working directory crawling
//!
//! This module provides functions to read file content and metadata
//! from Git commits without touching the working tree, as well as
//! enumerating files from the working directory for indexing uncommitted changes.

use anyhow::{Result, anyhow};
use gix::ObjectId;
use gix::objs::TreeRefIter;
use gix::traverse::tree::Recorder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub relative_path: String,
    pub blob_id: String,
}

pub fn resolve_commit_oid(repo_path: &Path, commit: &str) -> Result<String> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    Ok(commit_id.to_hex().to_string())
}

pub struct PackageIndex {
    package_name_by_dir: HashMap<String, String>,
}

impl PackageIndex {
    pub fn new() -> Self {
        Self {
            package_name_by_dir: HashMap::new(),
        }
    }

    pub fn find_package_name(&self, relative_path: &str) -> Option<&str> {
        let path = Path::new(relative_path);
        let mut current = path.parent().unwrap_or(path);

        loop {
            let dir_str = current.to_string_lossy();
            let dir_key = if dir_str == "." {
                String::new()
            } else {
                dir_str.replace('\\', "/")
            };

            if let Some(name) = self.package_name_by_dir.get(&dir_key) {
                return Some(name);
            }

            if current == Path::new("") || current == Path::new(".") {
                if let Some(name) = self.package_name_by_dir.get("") {
                    return Some(name);
                }
                break;
            }

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

pub fn enumerate_commit_tree(repo_path: &Path, commit: &str) -> Result<Vec<FileEntry>> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

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

    let tree_data = {
        let tree_obj = repo
            .find_object(tree_id)
            .map_err(|e| anyhow!("Failed to find tree object: {}", e))?;
        tree_obj.data.clone()
    };

    let mut recorder = Recorder::default();
    gix::traverse::tree::breadthfirst(
        TreeRefIter::from_bytes(&tree_data),
        &mut gix::traverse::tree::breadthfirst::State::default(),
        repo.objects,
        &mut recorder,
    )
    .map_err(|e| anyhow!("Failed to traverse tree: {}", e))?;

    Ok(recorder
        .records
        .into_iter()
        .filter(|entry| entry.mode.is_blob())
        .map(|entry| FileEntry {
            relative_path: entry.filepath.to_string(),
            blob_id: entry.oid.to_hex().to_string(),
        })
        .collect())
}

pub fn read_blob_content(repo_path: &Path, blob_id: &str) -> Result<Vec<u8>> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let object_id = ObjectId::from_hex(blob_id.as_bytes())
        .map_err(|e| anyhow!("Invalid blob ID '{}': {}", blob_id, e))?;

    let blob = repo
        .find_object(object_id)
        .map_err(|e| anyhow!("Failed to find blob '{}': {}", blob_id, e))?
        .try_into_blob()
        .map_err(|_| anyhow!("Object '{}' is not a blob", blob_id))?;

    Ok(blob.data.to_vec())
}

pub fn build_package_index_for_commit(repo_path: &Path, commit: &str) -> Result<PackageIndex> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

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

    let tree_data = {
        let tree_obj = repo
            .find_object(tree_id)
            .map_err(|e| anyhow!("Failed to find tree object: {}", e))?;
        tree_obj.data.clone()
    };

    let mut recorder = Recorder::default();
    gix::traverse::tree::breadthfirst(
        TreeRefIter::from_bytes(&tree_data),
        &mut gix::traverse::tree::breadthfirst::State::default(),
        repo.objects.clone(),
        &mut recorder,
    )
    .map_err(|e| anyhow!("Failed to traverse tree: {}", e))?;

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
                let filepath_str = String::from_utf8_lossy(filepath_bytes);
                let dir_path = filepath_str
                    .strip_suffix("/package.json")
                    .or_else(|| filepath_str.strip_suffix("package.json"))
                    .unwrap_or("")
                    .to_string();
                Some((dir_path, entry.oid))
            } else {
                None
            }
        })
        .collect();

    let mut index = PackageIndex::new();
    for (dir_path, blob_id) in package_json_entries {
        if let Ok(obj) = repo.find_object(blob_id) {
            if let Ok(blob) = obj.try_into_blob() {
                if let Some(name) = extract_package_name_from_bytes(&blob.data) {
                    index.package_name_by_dir.insert(dir_path, name);
                }
            }
        }
    }

    Ok(index)
}

#[derive(Deserialize)]
pub struct PackageJsonName {
    name: Option<String>,
}

/// Extract the "name" field from a package.json file content.
///
/// Uses proper JSON parsing (not string search) to handle edge cases
/// like nested "name" fields in other objects.
pub fn extract_package_name_from_bytes(content: &[u8]) -> Option<String> {
    serde_json::from_slice::<PackageJsonName>(content)
        .ok()?
        .name
        .filter(|name| !name.is_empty())
}

#[derive(Debug, Clone)]
pub struct WorkingDirEntry {
    pub relative_path: String,
    pub content_hash: String,
}

/// Enumerate files from the working directory.
///
/// This function walks the filesystem and returns all regular files with their
/// content hashes. Directory filtering is handled by passing the results through
/// the compiled crawl config's `should_crawl()` method.
///
/// Note: Only `.git` directories are explicitly excluded here. All other
/// filtering (node_modules, dist, etc.) should be handled by the crawl config.
pub fn enumerate_working_directory(repo_path: &Path) -> Result<Vec<WorkingDirEntry>> {
    use std::fs;

    let mut entries: Vec<WorkingDirEntry> = Vec::new();

    for entry in walkdir::WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let path = e.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();

            // Skip hidden directories except .git (we want to exclude .git contents)
            // Note: .git is a hidden directory, so we skip it entirely
            if name.starts_with('.') {
                return false;
            }

            true
        })
    {
        let entry = entry.map_err(|e| anyhow!("Failed to read directory entry: {}", e))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let relative_path = path
            .strip_prefix(repo_path)
            .map_err(|e| anyhow!("Failed to strip prefix: {}", e))?
            .to_string_lossy()
            .replace('\\', "/");

        let content = match fs::read(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  ⚠️  Skipping {} (can't read: {})", relative_path, e);
                continue;
            }
        };

        entries.push(WorkingDirEntry {
            relative_path,
            content_hash: compute_content_hash(&content),
        });
    }

    Ok(entries)
}

fn compute_content_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let result = hasher.finalize();
    format!("sha256:{}", hex::encode(result))
}

/// Build package index from the working directory.
///
/// This function walks the filesystem to find all package.json files and extracts
/// their package names. Only `.git` directories are explicitly excluded.
pub fn build_package_index_for_working_dir(repo_path: &Path) -> Result<PackageIndex> {
    let mut index = PackageIndex::new();

    for entry in walkdir::WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let path = e.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();

            // Skip hidden directories (including .git)
            if name.starts_with('.') {
                return false;
            }

            true
        })
    {
        let entry = entry.map_err(|e| anyhow!("Failed to read directory entry: {}", e))?;
        let path = entry.path();
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

        let dir_path = path
            .parent()
            .and_then(|p| p.strip_prefix(repo_path).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();

        if let Ok(content) = std::fs::read(path) {
            if let Some(name) = extract_package_name_from_bytes(&content) {
                index.package_name_by_dir.insert(dir_path, name);
            }
        }
    }

    Ok(index)
}

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
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let entries = enumerate_commit_tree(&repo_path, "HEAD").expect("Failed to enumerate");
        assert!(!entries.is_empty(), "Should have found some files");
        assert!(entries.iter().any(|e| e.relative_path == "README.md"));
        assert!(entries.iter().any(|e| e.relative_path == "Cargo.toml"));
    }

    #[test]
    fn test_read_blob_content_current_repo() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let entries = enumerate_commit_tree(&repo_path, "HEAD").expect("Failed to enumerate");
        let readme = entries
            .iter()
            .find(|e| e.relative_path == "README.md")
            .unwrap();
        let content = read_blob_content(&repo_path, &readme.blob_id).expect("Failed to read blob");
        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.contains("Monodex"));
    }

    #[test]
    fn test_build_package_index() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let _index =
            build_package_index_for_commit(&repo_path, "HEAD").expect("Failed to build index");
    }

    #[test]
    fn test_find_package_name_uses_repo_relative_paths() {
        let mut index = PackageIndex::new();
        index.package_name_by_dir.insert(
            "libraries/node-core-library".to_string(),
            "@rushstack/node-core-library".to_string(),
        );
        index
            .package_name_by_dir
            .insert("".to_string(), "root-package".to_string());

        assert_eq!(
            index.find_package_name("libraries/node-core-library/src/JsonFile.ts"),
            Some("@rushstack/node-core-library")
        );
        assert_eq!(
            index.find_package_name("libraries/node-core-library/package.json"),
            Some("@rushstack/node-core-library")
        );
        assert_eq!(index.find_package_name("src/main.rs"), Some("root-package"));
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
    fn test_extract_package_name_from_bytes_ignores_nested_name_fields() {
        let json = br#"{
  "exports": {
    ".": {
      "name": "nested-name-should-not-win"
    }
  },
  "name": "top-level-package"
}"#;
        let name = extract_package_name_from_bytes(json);
        assert_eq!(name, Some("top-level-package".to_string()));
    }

    #[test]
    fn test_nested_package_directory_key_round_trip() {
        let relative_package_json = "libraries/node-core-library/package.json";
        let dir_path = relative_package_json
            .strip_suffix("/package.json")
            .or_else(|| relative_package_json.strip_suffix("package.json"))
            .unwrap_or("");

        assert_eq!(dir_path, "libraries/node-core-library");

        let mut index = PackageIndex::new();
        index.package_name_by_dir.insert(
            dir_path.to_string(),
            "@rushstack/node-core-library".to_string(),
        );

        assert_eq!(
            index.find_package_name("libraries/node-core-library/src/JsonFile.ts"),
            Some("@rushstack/node-core-library")
        );
    }

    #[test]
    fn test_enumerate_working_directory() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let entries =
            enumerate_working_directory(&repo_path).expect("Failed to enumerate working directory");
        assert!(!entries.is_empty(), "Should have found some files");
        // README.md should be found (it's in the gitignore exclusion list but we only skip .git)
        // Actually README.md is a regular file that should be found
        assert!(entries.iter().any(|e| e.relative_path == "README.md"));
    }

    #[test]
    fn test_compute_content_hash() {
        let content = b"Hello, world!";
        let hash = compute_content_hash(content);
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), 71);
        assert_eq!(hash, compute_content_hash(content));
        assert_ne!(hash, compute_content_hash(b"Different content"));
    }
}
