# monodex Development Plan

## Overview

This plan implements label-based semantic indexing with incremental crawl. The work is organized into phases with clear dependencies and testable milestones.

---

## Phase 1: Git Operations Layer

**Goal:** Enable reading file content and package information from Git commits without touching the working tree.

### 1.1 Git Tree Enumeration

Create `src/engine/git_ops.rs`:

- [ ] Add `gix` dependency to Cargo.toml (pure Rust Git implementation)
- [ ] Implement `enumerate_commit_tree(repo_path, commit) -> Vec<FileEntry>`
  ```rust
  pub struct FileEntry {
      pub relative_path: String,
      pub blob_id: String,
  }
  ```
- [ ] Handle NUL-delimited output from `git ls-tree -r -z`
- [ ] Filter out non-blob entries
- [ ] Test with a sample commit in rushstack repo

### 1.2 Git Blob Content Reading

- [ ] Implement `read_blob_content(repo_path, blob_id) -> Result<Vec<u8>>`
- [ ] Use `git cat-file --batch` for batch reading (or gix equivalent)
- [ ] Handle UTF-8 decoding errors gracefully
- [ ] Test reading a few blobs from sample commit

### 1.3 Package Index Building

- [ ] Implement `build_package_index_for_commit(repo_path, commit) -> Result<PackageIndex>`
  - Enumerate all `package.json` files from commit tree
  - Batch-read blob contents
  - Parse JSON to extract `"name"` field
  - Build `HashMap<String, String>` mapping directory -> package name
- [ ] Implement `find_package_name_from_index(relative_path, index) -> Option<&str>`
  - Walk ancestor directories to find nearest package
- [ ] Test on rushstack commit, verify correct package resolution

### 1.4 Integration Test

- [ ] Verify we can enumerate a commit, resolve packages, and read content
- [ ] Confirm no dependency on working tree state

---

## Phase 2: Schema Changes

**Goal:** Update Qdrant payload schema to support labels and commit-based identity.

### 2.1 Update PointPayload Struct

In `src/engine/uploader.rs`:

- [ ] Add new fields to `PointPayload`:
  - `label_id: String`
  - `active_label_ids: Vec<String>`
  - `embedder_id: String`
  - `chunker_id: String`
  - `blob_id: String`
  - `package_name: String`
  - `chunk_ordinal: usize` (rename from `chunk_number`)
- [ ] Remove `file_id` field
- [ ] Keep `source_uri` but document it's not a key

### 2.2 Add LabelMetadata Struct

- [ ] Create `LabelMetadata` struct for label metadata points
- [ ] Use `source_type: "label-metadata"` as discriminator
- [ ] Point ID is the `label_id` string (direct lookup)
- [ ] Store zero-vector (768 dims of 0.0) - required by Qdrant but never used in search

### 2.3 Update Qdrant Operations

- [ ] Add `upsert_label_metadata(label: LabelMetadata)`
- [ ] Add `get_label_metadata(label_id) -> Option<LabelMetadata>`
- [ ] Add `add_label_to_chunk(chunk_id, label_id)` for updating `active_label_ids`
- [ ] Add `remove_label_from_chunks(label_id)` for cleanup scan

### 2.4 File and Chunk ID Computation

In `src/engine/util.rs`:

- [ ] Implement `compute_file_id(embedder_id, chunker_id, blob_id, relative_path) -> String`
  - This is a file-scoped identity (semantic version of a file)
  - Does NOT include chunk_ordinal
- [ ] Update `Chunk` struct to include `file_id` field
- [ ] Point ID for Qdrant = hash of (file_id, chunk_ordinal)

---

## Phase 3: Content-Based Chunking

**Goal:** Decouple chunking from filesystem paths, enable Git blob content as input.

### 3.1 Refactor Partitioner Interface

In `src/engine/partitioner.rs`:

- [ ] Create `chunk_content()` function that accepts content as string:
  ```rust
  pub fn chunk_content(
      content: &str,
      relative_path: &str,
      package_name: &str,
      target_size: usize,
  ) -> Result<Vec<Chunk>>
  ```
- [ ] Update `chunk_file()` to be a convenience wrapper that reads from filesystem

### 3.2 Update Chunk Metadata

- [ ] Ensure chunks include `blob_id` field
- [ ] Ensure chunks include `package_name` field
- [ ] Compute `file_id` during chunking using util function
- [ ] Point ID = hash of (file_id, chunk_ordinal)

### 3.3 Test Content-Based Chunking

- [ ] Test that same content + path produces same file_id
- [ ] Test that path changes produce different file_id (expected behavior)
- [ ] Test that same content at different paths = different chunks (semantic context matters)

---

## Phase 4: Label-Aware Crawler

**Goal:** Implement the full crawl flow with label support.

### 4.1 Crawl Command Updates

In `src/main.rs`:

- [ ] Add `--label` argument (required)
- [ ] Add `--commit` argument (defaults to HEAD)
- [ ] Update crawl flow to use Git operations layer

### 4.2 Crawl Implementation

- [ ] Resolve commit OID from `--commit` argument
- [ ] Build package index for commit
- [ ] Enumerate files from commit tree
- [ ] For each file:
  - Check if chunk already exists (sentinel check)
  - If exists and complete: update `active_label_ids` only
  - If not: read content, chunk, embed, upload
- [ ] Track all chunk IDs touched during crawl

### 4.3 Label Reassignment Cleanup

**Critical:** Only runs after fully successful crawl. Partial crawls must NOT trigger cleanup.

- [ ] Track all file IDs touched during crawl (HashSet)
- [ ] After successful crawl, scan for chunks with label in `active_label_ids`
- [ ] For each chunk, extract file_id
- [ ] If file_id NOT in touched set:
  - Remove label from `active_label_ids`
  - Delete chunk if `active_label_ids` becomes empty
- [ ] Ensure interrupted/failed crawls skip this step entirely

### 4.4 Label Metadata Persistence

- [ ] Upsert label metadata at start of crawl
- [ ] Update `crawl_complete` and timestamp at end

---

## Phase 5: Query Updates

**Goal:** Update search and view to work with label filtering.

### 5.1 Use Command (Default Context)

- [ ] Add `use` command to set default catalog and label
- [ ] Store default context in `~/.config/monodex/context.json`
- [ ] All commands check default context when flags not provided
- [ ] Explicit flags override default context

### 5.2 Search Command

- [ ] Add `--label` argument (uses default context if not provided)
- [ ] Filter by `active_label_ids CONTAINS label_id`
- [ ] Update output format to show `file_id:ordinal`

### 5.3 View Command

- [ ] Update to use file-oriented identity (file_id + selector)
- [ ] Filter by `active_label_ids`
- [ ] Support selector syntax (`:N`, `:N-M`, `:N-end`)
- [ ] Support file reconstruction (view all chunks via file_id without selector)
- [ ] Note: Path-based view is deferred to a later phase

### 5.4 Test Query Flow

- [ ] Crawl a label
- [ ] Set default context with `use`
- [ ] Search within that label (using default context)
- [ ] View specific chunks
- [ ] Verify results are correct

---

## Phase 6: Testing & Documentation

**Goal:** Ensure system works end-to-end and is documented.

### 6.1 Integration Testing

- [ ] Crawl multiple labels from same commit (verify dedup)
- [ ] Crawl same label from different commits (verify cleanup)
- [ ] Verify incremental crawl resumes after interruption

### 6.2 Update README.md

- [ ] Document new CLI commands
- [ ] Document label concept
- [ ] Update examples

### 6.3 Final Verification

- [ ] Delete old Qdrant collection
- [ ] Fresh crawl with new schema
- [ ] Verify all operations work

---

## Notes

### Dependencies

- `gix` crate for Git operations (pure Rust, avoids subprocess overhead)
- Existing `tree-sitter` for parsing
- Existing `ort` for ONNX embeddings

### File Structure

```
src/
├── main.rs                    # CLI entry point
└── engine/
    ├── mod.rs                 # Module exports
    ├── config.rs              # Config loading and file exclusion rules
    ├── chunker.rs             # File chunking dispatcher
    ├── partitioner.rs         # AST-based TypeScript chunking
    ├── markdown_partitioner.rs # Markdown heading-based chunking
    ├── parallel_embedder.rs   # Parallel embedding with multiple ONNX sessions
    ├── package_lookup.rs      # Package name resolution (walk up to package.json)
    ├── uploader.rs            # Qdrant HTTP client
    └── util.rs                # Hash utilities for chunk IDs
```

### Testing Strategy

Each phase should have a commit point where:
1. Code compiles
2. Basic functionality works
3. User can review the diff before proceeding

Do not proceed to next phase without user approval.
