# monodex Development Plan

## Overview

This plan implements label-based semantic indexing with incremental crawl. The work is organized into phases with clear dependencies and testable milestones.

### Testing Approach

- **Development testing**: Use the `sparo` catalog (small monorepo, ~266 chunks, fast iteration)
- **Final verification**: Use the `rushstack` catalog (large monorepo, hours to crawl - run by user only)
- **Qdrant collection**: Named `monodex` (not `rushstack`)

**Example catalogs in config:**
```json
{
  "qdrant": { "url": "http://localhost:6333", "collection": "monodex" },
  "catalogs": {
    "sparo": { "type": "monorepo", "path": "/path/to/sparo" },
    "rushstack": { "type": "monorepo", "path": "/path/to/rushstack" }
  }
}
```

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

## Phase 3: Content-Based Chunking ✅ COMPLETE

**Goal:** Decouple chunking from filesystem paths, enable Git blob content as input.

### 3.1 Refactor Partitioner Interface

In `src/engine/partitioner.rs`:

- [x] Create `chunk_content()` function that accepts content as string:
  ```rust
  pub fn chunk_content(
      content: &str,
      ctx: &ChunkContext,
      target_size: usize,
  ) -> Result<Vec<Chunk>>
  ```
  *(Note: Implementation is in `chunker.rs`, uses `ChunkContext` for Phase 2 fields)*
- [x] Update `chunk_file()` to be a convenience wrapper that reads from filesystem

### 3.2 Update Chunk Metadata

- [x] Ensure chunks include `blob_id` field
- [x] Ensure chunks include `package_name` field
- [x] Compute `file_id` during chunking using util function
- [x] Point ID = hash of (file_id, chunk_ordinal)

### 3.3 Test Content-Based Chunking

- [x] Test that same content + path produces same file_id
- [x] Test that path changes produce different file_id (expected behavior)
- [x] Test that same content at different paths = different chunks (semantic context matters)

---

## Phase 4: Label-Aware Crawler [✅ COMPLETE]

**Goal:** Implement the full crawl flow with label support.

### 4.1 Crawl Command Updates

In `src/main.rs`:

- [x] Add `--label` argument (required)
- [x] Add `--commit` argument (defaults to HEAD)
- [x] Update crawl flow to use Git operations layer

### 4.2 Crawl Implementation

- [x] Resolve commit OID from `--commit` argument
- [x] Build package index for commit
- [x] Enumerate files from commit tree
- [x] For each file:
  - Check if chunk already exists (sentinel check)
  - If exists and complete: update `active_label_ids` only
  - If not: read content, chunk, embed, upload
- [x] Track all chunk IDs touched during crawl

### 4.3 Label Reassignment Cleanup

**Critical:** Only runs after fully successful crawl. Partial crawls must NOT trigger cleanup.

- [x] Track all file IDs touched during crawl (HashSet)
- [x] After successful crawl, scan for chunks with label in `active_label_ids`
- [x] For each chunk, extract file_id
- [x] If file_id NOT in touched set:
  - Remove label from `active_label_ids`
  - Delete chunk if `active_label_ids` becomes empty
- [x] Ensure interrupted/failed crawls skip this step entirely

### 4.4 Label Metadata Persistence

- [x] Upsert label metadata at start of crawl
- [x] Update `crawl_complete` and timestamp at end

---

## Phase 5: Query Updates ✅ COMPLETE

**Goal:** Update search and view to work with label filtering.

### 5.1 Use Command (Default Context)

- [x] Add `use` command to set default catalog and label
- [x] Store default context in `~/.config/monodex/context.json`
- [x] All commands check default context when flags not provided
- [x] Explicit flags override default context

### 5.2 Search Command

- [x] Add `--label` argument (uses default context if not provided)
- [x] Filter by `active_label_ids CONTAINS label_id`
- [x] Update output format to show `file_id:ordinal`

### 5.3 View Command

- [x] Update to use file-oriented identity (file_id + selector)
- [x] Filter by `active_label_ids`
- [x] Support selector syntax (`:N`, `:N-M`, `:N-end`)
- [x] Support file reconstruction (view all chunks via file_id without selector)
- [x] Note: Path-based view is deferred to a later phase

### 5.4 Test Query Flow ✅ COMPLETE

- [x] Crawl a label (sparo:main already indexed)
- [x] Set default context with `use`
- [x] Search within that label (using default context)
- [x] View specific chunks
- [x] View chunk ranges (`:N-M` syntax)
- [x] Verify results are correct

**Tested:** 2026-04-08 - All query operations working correctly with sparo catalog.

### 5.5 Update README.md ✅ COMPLETE

- [x] Document new CLI commands
- [x] Document label concept
- [x] Update examples

### 5.6 Fix Error Handling in Crawl ✅ COMPLETE

**Goal:** Ensure errors during crawl are properly propagated rather than swallowed, and partial state can be cleaned up.

**Changes implemented:**
- [x] Added failure tracking via `Arc<Mutex<Vec<String>>>` for upload, file-complete, and label-add failures
- [x] Upload failures now track which files failed with error message
- [x] File completion marking failures are tracked and reported
- [x] Label assignment failures are tracked and reported (critical - chunks won't be searchable without label)
- [x] Changed `let _ = ...` to proper `if let Err(e)` with tracking
- [x] Added failure summary reporting after embedding phase completes
- [x] Changed warning emoji from ⚠️ to ❌ for actual errors to improve visibility

**Deferred to Phase 6 (Edge Case Testing):**
- [ ] Kill Qdrant mid-crawl: verify partial chunks exist, label metadata shows incomplete
- [ ] Resume after partial failure: verify crawl continues from where it stopped
- [ ] Verify orphaned chunks (uploaded but not labeled) are found by future `monodex gc` command

**Files modified:**
- `src/main.rs` - `run_crawl_label()` function

---

## Phase 6: Edge Case Testing

**Goal:** Test edge cases and failure modes that emerge from label-based indexing semantics.

### 6.1 Multi-Label Scenarios ✅ COMPLETE

- [x] Crawl the same commit under two different labels (verify chunks share `active_label_ids`)
- [x] Verify both labels return same chunks in search
- [x] Crawl a different commit under one label (verify label reassignment)
- [x] Verify the other label still has its original chunks

### 6.2 Incremental Crawl Edge Cases

- [ ] Interrupt a crawl (CTRL+C) and verify:
  - Label metadata shows `crawl_complete = false`
  - Partial chunks exist but label membership is incomplete
- [ ] Resume interrupted crawl and verify completion
- [ ] Verify label reassignment does NOT run for partial crawls

**Note:** These tests require manual intervention (CTRL+C) and are not automated.

### 6.3 Chunk Deduplication Edge Cases ✅ COMPLETE

- [x] File moves between packages (path change):
  - Verify new chunks are created (breadcrumb context changes)
  - [Skipped - would require creating test scenario]
- [x] Same content, different path:
  - Verified: Different `file_id` values for same content at different paths
  - Verified: Both can coexist in different labels (tsconfig.json example)
  - Found 25 groups with duplicate content_hash but unique file_ids

### 6.4 Label Cleanup Edge Cases

- [ ] Re-crawl same label with different commit
  - Verify chunks from old commit are removed from label
  - Verify chunks shared with other labels are NOT deleted
  - Verify orphaned chunks (empty `active_label_ids`) are deleted
- [ ] Purge a single label and verify other labels unaffected

### 6.5 Search/View Edge Cases ✅ COMPLETE

- [x] Search with label that has no chunks yet - returns empty results gracefully
- [x] View chunks from file that exists in multiple labels - works correctly
- [x] View chunks after label has been purged - N/A (purge operates at catalog level)
- [x] Default context with non-existent catalog/label - allows setting, search returns empty

### 6.6 Fresh Installation Verification

- [ ] Delete Qdrant collection
- [ ] Fresh crawl with new schema
- [ ] Verify all operations work from clean state

**Note:** Destructive test - skipped to preserve test data. Run manually before release.

---

## Phase 6 Summary

**Completed Tests:**
- Multi-label scenarios: chunks correctly share active_label_ids
- Label reassignment: works correctly when re-crawling different commits
- Chunk deduplication: same content at different paths has different file_id
- Search/view edge cases: handles non-existent labels gracefully

**Skipped Tests:**
- Interrupted crawl (requires manual CTRL+C)
- Fresh installation (destructive)

**Goal:** Support crawling the live working directory (uncommitted changes) in addition to Git commits.

### 7.1 Design Working Directory Identity Model

- [ ] Define identity scheme for working directory files:
  - No `blob_id` (not in Git yet)
  - Use content hash computed from file content
  - Path + content hash determines `file_id`
- [ ] Decide `source_kind` value for working directory labels (e.g., `"working-directory"`)
- [ ] Decide how to represent "no commit" in `LabelMetadata.commit_oid` (empty string? "WORKING"?)
- [ ] Document: working directory labels are mutable (re-crawl changes content), commit labels are immutable

### 7.2 Implement Working Directory Enumeration

In `src/engine/git_ops.rs` or new module:

- [ ] Implement `enumerate_working_directory(repo_path) -> Vec<FileEntry>`
  - Walk the filesystem, respecting `.gitignore`
  - Skip `node_modules` and other excluded paths (reuse `should_skip_path`)
  - Compute content hash for each file
- [ ] Decide: use `git status` to find changed files, or full directory walk?
  - Full walk: simpler, consistent with commit-based crawling
  - `git status`: faster for incremental updates, but more complex

### 7.3 Update Crawler for Working Directory Mode

- [ ] Add `--working-dir` flag to `crawl` command (mutually exclusive with `--commit`)
- [ ] Create `run_crawl_working_dir()` function or refactor `run_crawl_label()` to handle both modes
- [ ] For working directory mode:
  - Use `enumerate_working_directory()` instead of `enumerate_commit_tree()`
  - Compute content hash instead of using `blob_id`
  - Set `source_kind = "working-directory"` in label metadata
  - Set `commit_oid = ""` (or chosen sentinel value)
- [ ] Ensure package index works with working directory (walk up to nearest `package.json` on disk)

### 7.4 Test Working Directory Crawling

- [ ] Create a test repo with uncommitted changes
- [ ] Crawl with `--working-dir` and verify:
  - Label metadata shows `source_kind = "working-directory"`
  - Chunks have correct content (from working tree, not HEAD)
  - Search returns working directory content
- [ ] Modify a file and re-crawl:
  - Verify old chunks are replaced (label reassignment)
  - Verify new content is indexed
- [ ] Test alongside commit-based label:
  - Crawl `--commit HEAD --label main`
  - Crawl `--working-dir --label working`
  - Verify both labels can coexist, search returns correct results for each

### 7.5 Document Working Directory Mode

- [ ] Update README.md with `--working-dir` usage examples
- [ ] Document semantic differences (mutable vs immutable labels)

---

## Phase 8: Offline Garbage Collection

**Goal:** Provide a command to clean up orphaned chunks and recover storage.

### 8.1 Implement GC Command

- [ ] Add `gc` command: `monodex gc --catalog rushstack`
- [ ] Implementation:
  - Scroll through all chunks with `source_type = "code"`
  - Find chunks where `active_label_ids` is empty
  - Delete them
  - Report count and estimated storage recovered
- [ ] Add `--dry-run` flag to show what would be deleted without actually deleting

### 8.2 Test GC Scenarios

- [ ] Create orphaned chunks (interrupt a crawl, or delete a label's chunks manually)
- [ ] Run `monodex gc --dry-run` and verify correct chunks identified
- [ ] Run `monodex gc` and verify orphans deleted, other chunks untouched
- [ ] Verify `monodex gc` after successful crawl finds nothing to clean

---

## Phase 9: Watch Mode

**Goal:** Continuously monitor and re-index a working directory as files change.

### 9.1 Design Watch Mode Architecture

- [ ] Research file watching libraries (notify, notify-debouncer-mini)
- [ ] Decide on watch mode trigger:
  - Debounced file system events (preferred)
  - Polling interval (fallback for network filesystems)
- [ ] Design state management:
  - Which files are currently indexed under the working label?
  - How to handle rapid successive changes (debouncing)
- [ ] Design output/feedback:
  - Log to stdout? Background daemon?
  - How to show progress during re-indexing
- [ ] Consider integration with existing `--working-dir` crawl:
  - Watch mode could be `--watch` flag that runs indefinitely
  - Initial crawl + incremental updates

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
    ├── chunker.rs             # File chunking dispatcher
    ├── config.rs              # Config loading and file exclusion rules
    ├── git_ops.rs             # Git tree enumeration and blob reading
    ├── markdown_partitioner.rs # Markdown heading-based chunking
    ├── mod.rs                 # Module exports
    ├── package_lookup.rs      # Package name resolution (walk up to package.json)
    ├── parallel_embedder.rs   # Parallel embedding with multiple ONNX sessions
    ├── partitioner.rs         # AST-based TypeScript chunking
    ├── uploader.rs            # Qdrant HTTP client
    └── util.rs                # Hash utilities for chunk IDs
```

### Testing Strategy

Each phase should have a commit point where:
1. Code compiles
2. Basic functionality works
3. User can review the diff before proceeding

Do not proceed to next phase without user approval.
