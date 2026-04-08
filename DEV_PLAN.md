# monodex Development Plan

## Overview

This plan implements label-based semantic indexing with incremental crawl. The work is organized into phases with clear dependencies and testable milestones.

---

## Phase 1: Git Operations Layer

**Goal:** Enable reading file content and package information from Git commits without touching the working tree.

### 1.1 Git Tree Enumeration

Create `src/engine/git_ops.rs`:

- [x] Add `gix` dependency to Cargo.toml (pure Rust Git implementation)
- [x] Implement `enumerate_commit_tree(repo_path, commit) -> Vec<FileEntry>`
  ```rust
  pub struct FileEntry {
      pub relative_path: String,
      pub blob_id: String,
  }
  ```
- [x] Handle NUL-delimited output from `git ls-tree -r -z`
- [x] Filter out non-blob entries
- [x] Test with a sample commit in rushstack repo

### 1.2 Git Blob Content Reading

- [x] Implement `read_blob_content(repo_path, blob_id) -> Result<Vec<u8>>`
- [x] Use `git cat-file --batch` for batch reading (or gix equivalent)
- [x] Handle UTF-8 decoding errors gracefully
- [x] Test reading a few blobs from sample commit

### 1.3 Package Index Building

- [x] Implement `build_package_index_for_commit(repo_path, commit) -> Result<PackageIndex>`
  - Enumerate all `package.json` files from commit tree
  - Batch-read blob contents
  - Parse JSON to extract `"name"` field
  - Build `HashMap<String, String>` mapping directory -> package name
- [x] Implement `find_package_name_from_index(relative_path, index) -> Option<&str>`
  - Walk ancestor directories to find nearest package
- [x] Test on rushstack commit, verify correct package resolution

### 1.4 Integration Test

- [x] Verify we can enumerate a commit, resolve packages, and read content
- [x] Confirm no dependency on working tree state

---

## Phase 2: Schema Changes ✅ COMPLETE

**Goal:** Update Qdrant payload schema to support labels and commit-based identity.

### 2.1 Update PointPayload Struct

In `src/engine/uploader.rs`:

- [x] Add new fields to `PointPayload`:
  - `label_id: String`
  - `active_label_ids: Vec<String>`
  - `embedder_id: String`
  - `chunker_id: String`
  - `blob_id: String`
  - `package_name: String`
  - `chunk_ordinal: usize` (rename from `chunk_number`)
- [x] Keep `file_id` field (now a 16-char hex string instead of u64)
- [x] Keep `source_uri` but document it's not a key

### 2.2 Add LabelMetadata Struct

- [x] Create `LabelMetadata` struct for label metadata points
- [x] Use `source_type: "label-metadata"` as discriminator
- [x] Point ID is the `label_id` string (direct lookup)
- [x] Store zero-vector (768 dims of 0.0) - required by Qdrant but never used in search

### 2.3 Update Qdrant Operations

- [x] Add `upsert_label_metadata(label: LabelMetadata)`
- [x] Add `get_label_metadata(label_id) -> Option<LabelMetadata>`
- [x] Add `add_label_to_chunk(chunk_id, label_id)` for updating `active_label_ids`
- [x] Add `add_label_to_file_chunks(file_id, label_id)` for batch file updates
- [x] Add `remove_label_from_chunks(label_id)` for cleanup scan
- [x] Add `get_file_sentinel(file_id, label_id)` to check if file already indexed
- [x] Add `set_active_labels(chunk_id, label_ids)` for atomic replacement
- [x] Add `delete_point(chunk_id)` for cleanup
- [x] Add `search_with_label(query, label_id)` for label-filtered search
- [x] Add `get_chunks_by_file_id_with_label(file_id, label_id)` for label-filtered view

### 2.4 File and Chunk ID Computation

In `src/engine/util.rs`:

- [x] Implement `compute_file_id(embedder_id, chunker_id, blob_id, relative_path) -> String`
  - This is a file-scoped identity (semantic version of a file)
  - Does NOT include chunk_ordinal
  - Returns 16-char hex string
- [x] Add `EMBEDDER_ID` and `CHUNKER_ID` constants
- [x] Implement `compute_point_id(file_id, chunk_ordinal) -> String`
  - Returns `<file_id>:<ordinal>` format for deterministic chunk IDs
- [x] Implement `compute_label_id(catalog, label_name) -> String`
  - Returns `<catalog>:<label_name>` format
- [x] Update `Chunk` struct to include new Phase 2 fields
- [x] Add `ChunkContext` struct for content-based chunking
- [x] Add `chunk_content()` function for Git blob-based chunking
- [x] Add `Chunk::point_id()` method

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

- [x] Ensure chunks include `blob_id` field
- [x] Ensure chunks include `package_name` field
- [x] Compute `file_id` during chunking using util function
- [x] Point ID = hash of (file_id, chunk_ordinal)

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
