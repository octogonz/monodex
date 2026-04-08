# monodex Design Document

## Testing Note

- **Development testing**: Use the `sparo` catalog (small monorepo, fast iteration)
- **Final verification**: Use the `rushstack` catalog (large monorepo, hours to crawl)
- **Qdrant collection**: Named `monodex` (not `rushstack`)

## Overview

monodex is a semantic search indexer for Rush monorepos, using Qdrant vector database with local embeddings. It supports label-based semantic indexing where each label defines a queryable fileset (like a Git commit or branch head) within a catalog.

---

## Core Concepts

### Label-Based Indexing

A **label** is a named, queryable fileset within a catalog. Examples:
- `catalog = rushstack`
- `label = main`
- `label = feature-x`

**label_id** is the fully qualified identity: `<catalog>:<label_name>`
- Example: `rushstack:main`, `rushstack:feature-x`

A search is scoped by both catalog and label.

### Content vs Membership

**Key principle:**
- **Chunks** = immutable content (text, embeddings, metadata)
- **Labels** = mutable membership (which chunks belong to which queryable fileset)

When a label is refreshed to a new commit:
- Existing chunks remain (immutable)
- Membership (`active_label_ids`) is updated (mutable)
- Orphaned chunks (no labels) can be garbage collected

This separation allows efficient branch switching without re-embedding identical content.

### Commit-Based Crawling

For Git-backed code catalogs, crawling reads from Git objects, not the working tree:
- Enumerate files from the commit tree (`git ls-tree`)
- Read file content from Git blobs (`git cat-file --batch`)
- Deterministic and reproducible
- Ignores uncommitted working tree changes

### Context Over Dedup

Identical file content may require re-crawl when contextual identity changes:
- `blob_id`: identity of raw Git file content (provenance, diagnostics)
- `chunk_id`: identity of the indexed semantic artifact (depends on context)

We optimize for switching between Git branches with overlap, NOT for generalized pattern matching or data compression. Path renames that affect breadcrumb context will create new chunks.

### Qdrant as Authoritative State

Qdrant is the only authoritative state store:
- Label metadata lives in Qdrant
- File completion state lives in Qdrant
- Label membership lives in Qdrant

No Git refs, JSON sidecars, or SQLite in this phase.

---

## Schema

### Point Payload (Code Chunks)

```rust
pub struct PointPayload {
    pub text: String,
    pub source_type: String,          // "code"
    
    // Label membership
    pub catalog: String,
    pub label_id: String,             // Transitional: the initiating label. Prefer active_label_ids.
    pub active_label_ids: Vec<String>, // All labels this chunk belongs to (authoritative)
    
    // Implementation identity
    pub embedder_id: String,          // e.g., "jina-embeddings-v2-base-code:v1"
    pub chunker_id: String,           // e.g., "typescript-partitioner:v1"
    
    // Provenance
    pub blob_id: String,              // Git blob SHA
    pub content_hash: String,         // Hash of chunk text
    
    // File identity
    pub file_id: String,              // Semantic file identity (for grouping chunks)
    
    // Path context (for retrieval without Git)
    pub relative_path: String,
    pub package_name: String,
    pub source_uri: String,           // Useful for locating in Git/GitHub, but NOT a key
    
    // Chunk metadata
    pub chunk_ordinal: usize,         // 1-indexed position in file
    pub chunk_count: usize,
    pub start_line: usize,
    pub end_line: usize,
    
    // Semantic context
    pub symbol_name: Option<String>,
    pub chunk_type: String,           // AST node type: function, class, method, etc.
    pub chunk_kind: String,           // content, imports, changelog, config
    pub breadcrumb: Option<String>,   // Human-readable: package:File.ts:Symbol
    
    // Sentinel for incremental crawl
    pub file_complete: bool,          // Only true on chunk_ordinal=1
}
```

**Field notes:**
- `source_uri`: Best-effort display/debug locator for Git/GitHub links. Not guaranteed stable or canonical. Not a key.
- `chunk_ordinal`: Renamed from `chunk_number` for clarity. Always use `chunk_ordinal`.
- `file_id`: Semantic file identity for grouping chunks. Used for sentinel checks and file-level operations.
- `label_id`: Transitional field. Prefer `active_label_ids` for label membership queries.

### Label Metadata

Label metadata is stored as special points in the main Qdrant collection:

```rust
pub struct LabelMetadata {
    pub source_type: String,          // "label-metadata"
    pub catalog: String,
    pub label_id: String,             // e.g., "rushstack:main"
    pub label_name: String,           // e.g., "main"
    pub commit_oid: String,           // Resolved commit SHA
    pub source_kind: String,          // "git-commit"
    pub crawl_complete: bool,
    pub updated_at_unix_secs: u64,
}
```

**Point ID:** The `label_id` string is used directly as the point ID, allowing direct lookup.

**Vector:** Metadata points store a zero-vector of exactly 768 dimensions (matching the collection's vector size): `[0.0; 768]`. Qdrant requires vectors for all points, but these points are never used in similarity search. The dimension MUST match the collection's configured vector size to avoid insertion errors.

**Why single collection:** Using the main collection (rather than a separate metadata collection) avoids managing multiple Qdrant collections and keeps all state in one place. The tradeoff is mixing vector-bearing chunk records with metadata-only records. This is acceptable because:
- The `source_type` discriminator clearly separates them
- Metadata points are few (one per label) compared to millions of chunks
- Query code filters by `source_type` when needed

**ID semantics note:** This introduces mixed point ID semantics:
- Chunks: hash-derived hex strings (e.g., `700a4ba232fe9ddc`)
- Labels: meaningful strings (e.g., `rushstack:main`)

Both are valid Qdrant string IDs. The tradeoff is less uniformity, but direct label lookup is convenient.

### Qdrant Point IDs

Point IDs for code chunks use deterministic hashes:

```rust
pub fn compute_file_id(
    embedder_id: &str,
    chunker_id: &str,
    blob_id: &str,
    relative_path: &str,
) -> String
```

The **file ID** represents a semantic version of a file. Individual chunks are identified by `(file_id, chunk_ordinal)`.

**Point ID formula:**
```rust
point_id = hash(file_id + chunk_ordinal)
```

This allows upsert-by-ID semantics: if the same file content at the same path is crawled under multiple labels, we update `active_label_ids` rather than creating duplicates.

**Important:** Reuse only occurs when ALL of these match:
- Same content (blob_id)
- Same path (relative_path)
- Same implementation (embedder_id, chunker_id)

Path changes will produce new chunks even if content is identical. This is intentional: semantic context outweighs deduplication.

---

## File and Chunk Identity

### Two-Level Model

**File Identity** (computed once per file):
```
file_id = hash(embedder_id + chunker_id + blob_id + relative_path)
```

**Chunk Identity** (file_id + ordinal):
```
chunk identity = (file_id, chunk_ordinal)
```

This is a file-oriented model: the ID identifies a semantic version of a file, and chunk ordinal selects within it.

### Requirements

- Depends on implementation semantics (embedder_id, chunker_id)
- Depends on content (blob_id)
- Depends on path context (relative_path affects breadcrumb)
- Stable across sessions and machines

### Why Path Is In The Identity

**Explicitly stated:** Path and breadcrumb context are semantically meaningful. This design:
- Does NOT optimize for reuse across path moves
- Does optimize for switching between Git branches with overlapping files
- Accepts that path renames will create new chunks

If a file moves from `libraries/foo/src/A.ts` to `libraries/bar/src/A.ts`, the breadcrumb changes from `@scope/foo:A.ts:Symbol` to `@scope/bar:A.ts:Symbol`. These are different semantic contexts, so different chunks is correct behavior.

### Why blob_id Is Separate

Useful for provenance, diagnostics, and future optimization opportunities. But not the full identity of the indexed artifact because context matters.

### Sentinel-Based Incremental Check

The **sentinel** is chunk 1 of a file:
- Point ID = hash of (file_id, chunk_ordinal=1)
- `file_complete = true` only on chunk 1
- Existence check = direct lookup of sentinel point ID

A file is considered fully indexed when:
1. Sentinel exists (chunk_ordinal = 1)
2. Sentinel has `file_complete = true`
3. `chunk_count` on sentinel indicates total chunks

This preserves resumable crawl semantics.

---

## Implementation Constants

Source-defined identifiers for the embedder and chunker:

```rust
const EMBEDDER_ID: &str = "jina-embeddings-v2-base-code:v1";
const CHUNKER_ID: &str = "typescript-partitioner:v1";
```

If behavior changes in a way that should invalidate reuse, the constant changes. These are not user-authored config values.

---

## Crawler Flow

### Step 1: Resolve Label Target

1. Resolve `--commit` to a full 40-character commit SHA (e.g., using `git rev-parse`)
2. Compute `label_id = <catalog>:<label>`
3. Upsert label metadata with `crawl_complete = false` (in-progress state)
   - This marks the crawl as in-progress before any work begins

### Step 2: Enumerate Files from Commit

1. Run `git ls-tree -r -z <commit>` to enumerate all files
2. Filter for files that pass catalog's path filtering rules
3. For each file, obtain `blob_id`, `relative_path`

### Step 3: Build Package Index

Build a fast package lookup for the commit:

```rust
pub struct PackageIndex {
    pub package_name_by_dir: HashMap<String, String>,
}
```

See "Git Package Index" section for implementation details.

### Step 4: Process Each File

For each file:
1. Resolve `package_name` using package index
2. Read content from Git blob
3. Chunk content using the implementation identified by `chunker_id`
4. Compute chunk payloads with path/package/breadcrumb context
5. Derive chunk identity using embedder_id, chunker_id, blob_id, relative_path, chunk_ordinal

### Step 5: Incremental Existence Check

For each file:
1. Compute `file_id`
2. **Lookup sentinel point by (file_id, chunk_ordinal=1)**:
   - Point ID = `hash(file_id + chunk_ordinal=1)`
   - Query Qdrant for chunk with `file_id` AND `chunk_ordinal = 1` AND `source_type = "code"`
3. If sentinel exists and `file_complete = true`:
   - Skip re-embedding
   - **Retrieve all chunks for file by filtering on `file_id`** (with `source_type = "code"`)
   - Add label to `active_label_ids` for each chunk (if not present)
4. If sentinel does not exist or not complete:
   - Read content from Git blob
   - Chunk and embed all chunks
   - Compute point ID for each chunk: `hash(file_id + chunk_ordinal)`
   - Upsert all chunks
   - Mark sentinel `file_complete = true`
   - Add label to `active_label_ids` for each chunk

### Step 6: Label Reassignment Cleanup

**Critical:** This step runs ONLY after a fully successful crawl completion. Partial crawls must NOT trigger reassignment.

1. Track all file IDs touched during crawl (in a HashSet)
2. Scan all chunks where `active_label_ids` contains the label
   - Filter: `source_type = "code"` (exclude metadata points)
3. For each chunk:
   - Extract the `file_id` field from the payload
   - If file_id NOT in touched set:
     - Remove label from `active_label_ids`
     - If `active_label_ids` becomes empty, delete the chunk

**Failure behavior:** If the crawl is interrupted or fails:
- Do NOT run reassignment
- Label may temporarily have stale chunks (acceptable)
- Next successful crawl will clean up

### Step 7: Finalize Label Metadata

When crawl completes successfully:
- Mark `crawl_complete = true`
- Store resolved commit OID
- Store update timestamp

---

## Git Package Index

### Goal

Given a Git commit, efficiently build a mapping from directory paths to package names:

```rust
HashMap<String, String>
// "libraries/node-core-library" -> "@rushstack/node-core-library"
```

### Strategy

1. Enumerate all `package.json` entries with `git ls-tree -r -z`
2. Batch-read blob contents with `git cat-file --batch`
3. Parse JSON to extract `"name"` field

### Implementation

```rust
pub fn build_package_index_for_commit(
    repo_root: &std::path::Path,
    commit: &str,
) -> anyhow::Result<PackageIndex>
```

**Key details:**
- Keys are repo-relative directory paths (e.g., `"libraries/node-core-library"`)
- This ensures portability and independence from filesystem location
- For repo-root package.json, key is empty string `""`

**Git protocol:**
- `git ls-tree -r -z <commit>` returns NUL-delimited entries: `<mode> <type> <object_id>\t<path>\0`
- `git cat-file --batch` returns for each blob: `<oid> <type> <size>\n<raw bytes>\n`

### Lookup During Crawl

For a file path like `libraries/node-core-library/src/JsonFile.ts`, check directories in order:
1. `libraries/node-core-library/src`
2. `libraries/node-core-library` (match found here)
3. `libraries`
4. `""` (repo root)

Return first match. This reproduces "nearest ancestor package.json governs the file".

```rust
pub fn find_package_name_from_index(
    relative_path: &str,
    package_index: &PackageIndex,
) -> Option<&str>
```

### Performance

- One full tree enumeration
- One long-lived `git cat-file --batch` process
- No per-file `git show`
- No filesystem traversal

---

## Query Interface

### Default Context

The `use` command sets a default catalog and label context to avoid repeating flags:

```bash
monodex use --catalog rushstack --label main
```

After running this, subsequent commands use the default context:

```bash
# Instead of:
monodex search --catalog rushstack --label main --text "query"

# You can run:
monodex search --text "query"
```

**Default context storage:** Stored in `~/.config/monodex/context.json`:
```json
{
  "catalog": "rushstack",
  "label": "main"
}
```

**Priority:** Explicit `--catalog` / `--label` flags override default context.

### Search

```bash
monodex search --catalog rushstack --label main --text "how does package lookup work?"
```

Qdrant filter:
```
source_type == "code"
AND catalog == "rushstack"
AND active_label_ids CONTAINS "rushstack:main"
```

**Important:** All search queries must filter `source_type = "code"` to exclude label metadata points from results.

### View

```bash
monodex view --id <file_id>[:<selector>]
```

Selector syntax:
- `:N` — single chunk N
- `:N-M` — chunks N through M
- `:N-end` — chunk N through last

Chunks are filtered by `active_label_ids` and sorted by `chunk_ordinal`.

**File reconstruction:** To reconstruct an entire file, view all chunks using the file_id without a selector. Order chunks by `chunk_ordinal` to reconstruct the original file content.

**Filtering:** View queries must filter `source_type = "code"` to exclude label metadata points.

**Note:** Path-based view (querying by `--path` instead of `--id`) is intentionally deferred to a later phase. The primary workflow is search → view using file IDs from search results.

### Crawl

```bash
monodex crawl --catalog rushstack --label main --commit HEAD
```

### CLI Surface

| Command | Purpose |
|---------|---------|
| `use` | Set default catalog/label context |
| `crawl` | Index a commit into a label |
| `search` | Semantic search within a label |
| `view` | View chunks by file ID |

All commands respect the default context set by `use`, but explicit flags override defaults.

---

## Embedding Model

### Current: jina-embeddings-v2-base-code

| Property | Value |
|----------|-------|
| Max tokens | **8192** |
| Dimensions | 768 |
| Model size | ~612MB (FP32 ONNX) |
| License | Apache 2.0 |
| Trained on | **Code + documentation** |

---

## Chunking Algorithm

### Goal

Divide a file into chunks that fit the embedding budget (6000 chars), splitting only at meaningful AST boundaries.

### Two Worlds Model

**Chunk Land (sizing/selection):**
- File as sequence of line ranges
- Measures size, knows budget
- Simple bookkeeping

**AST Land (structure/meaning):**
- Walks syntax tree
- Provides candidate split points at semantic boundaries
- No opinions about sizes

### Coordination

1. Start with one chunk = entire file
2. While any chunk exceeds budget:
   - Find meaningful split points from AST
   - Split at the point that best balances sizes
3. Done

### Quality Markers

- No marker: Good AST split
- `:[degraded-ast-split]`: AST split with poor geometry (tiny chunks)
- `:[fallback-split]`: No AST split found, used line-based recovery (failure mode)

---

## Backlog Issues

### Early Exit on Embedding Error Skips Flush

The `try_for_each(...)?` pattern exits early without flushing remaining chunks. Need cleanup wrapper to ensure:
- `stop_flag` is set
- Channels are closed
- Uploader thread joins
- Remaining chunks flush

### Upload Failures Treated as Success

`upload_batch()` errors are only logged, not propagated. Need retry or abort logic.

### Unbounded Write Batching

Crawl accumulates points for 60 seconds with no size limit. Need batch constraints.

### Files Deleted Before Replacement Indexing Succeeds

For changed files, existing chunks are deleted before re-indexing. If chunking/embedding/upload fails, file is permanently missing. Consider "replace after success" pattern.

### Orphaned Chunks When Sentinel Missing

Files missing chunk 1 are invisible to catalog view but orphaned chunks remain. Need garbage collection.

---

## Garbage Collection

### Inline vs Offline

**Inline cleanup** (during crawl):
- Label reassignment removes stale label membership
- Chunks with empty `active_label_ids` are deleted
- Runs automatically after successful crawl

**Offline GC** (separate command, future):
```bash
monodex gc --catalog rushstack
```
- Scan for chunks with empty `active_label_ids`
- Delete orphaned chunks
- Report storage recovered
- Useful for cleanup after interrupted crawls or manual operations

---

## Scale Expectations

| Metric | Estimate |
|--------|----------|
| Files per catalog | ~200,000 |
| Chunks per catalog | ~600,000 |
| Chunks per file | 1-20 (avg 3) |
| Embedding time | ~12ms per chunk (parallel) |
| Full crawl time | ~15-30 minutes |

---

## Catalog Resolution

### Config File

`~/.config/monodex/config.json`:

```json
{
  "qdrant": {
    "url": "http://localhost:6333",
    "collection": "monodex"
  },
  "catalogs": {
    "sparo": {
      "type": "monorepo",
      "path": "/path/to/sparo"
    },
    "rushstack": {
      "type": "monorepo",
      "path": "/path/to/rushstack"
    }
  }
}
```

**Note:** Use `sparo` for development testing. `rushstack` is for final verification only.

**File extension:** All config files use `.json` extension (not `.jsonc`) per Rush Stack conventions.

### Catalog to Repo Mapping

- `catalog` is a user-defined name in config
- For Git operations, the `path` field points to the repository root
- Future: `repo_id` could be derived from Git remote for cross-machine identity

---

## Working Directory Crawling

### Overview

Working directory crawling indexes uncommitted changes from the filesystem rather than Git objects. This is useful for:

- Indexing work-in-progress before committing
- Comparing uncommitted changes with committed code
- AI assistants that need to understand the current state of the codebase

### Identity Model

Working directory files use a different identity model than commit-based files:

| Property | Commit-Based | Working Directory |
|----------|-------------|-------------------|
| `blob_id` | Git blob SHA | `sha256:<hash>` (content hash) |
| `commit_oid` | Resolved commit SHA | `""` (empty string) |
| `source_kind` | `"git-commit"` | `"working-directory"` |

**Key insight:** The `file_id` is computed from `(embedder_id, chunker_id, blob_id, relative_path)`. For working directory files, the "blob_id" is actually a content hash. This means:

- Same content at same path → same `file_id` (can share chunks)
- Different content at same path → different `file_id` (new chunks)
- Same content at different path → different `file_id` (breadcrumb context matters)

### Label Metadata

```rust
LabelMetadata {
    source_kind: "working-directory".to_string(),
    commit_oid: "".to_string(),  // No commit
    crawl_complete: true,
    // ... other fields
}
```

### Mutability

Working directory labels are **mutable**:

- Re-crawling updates indexed content based on current filesystem state
- Content hash changes trigger new chunks
- Label reassignment removes stale chunks

Commit-based labels are **immutable** (for a given commit):

- Re-crawling the same commit is idempotent
- Same commit always produces same chunks

### Usage

```bash
# Index working directory
monodex crawl --catalog rushstack --label working --working-dir

# Search working directory content
monodex search --text "uncommitted feature" --label rushstack:working

# Compare with committed code
monodex search --text "same query" --label rushstack:main
monodex search --text "same query" --label rushstack:working
```

---

## Crawl Configuration

### Overview

Crawl policy (file types, exclusions, overrides) is externalized from Rust code into a JSON config file. This enables:
- Per-repo customization without code changes
- Easy sharing of configs between repos or teams
- Deterministic, debuggable behavior

### Config File Format

File: `monodex-crawl.json` (JSON format, `.json` extension per Rush Stack conventions)

```json
{
  "version": 1,
  "fileTypes": {
    ".ts": "typescript",
    ".tsx": "typescript",
    ".md": "markdown",
    ".json": "simpleLine"
  },
  "patternsToExclude": [
    "node_modules/",
    "dist/",
    "build/",
    "lib/",
    "*.snap",
    "*.test.ts",
    "*.spec.ts",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock"
  ],
  "patternsToKeep": [
    "src/",
    "test/"
  ]
}
```

### Fields

| Field | Required | Description |
|-------|----------|-------------|
| `version` | Yes | Config schema version (must be `1`) |
| `fileTypes` | Yes | Map of file suffix → chunking strategy |
| `patternsToExclude` | Yes | Array of glob patterns for paths to skip |
| `patternsToKeep` | Yes | Array of glob patterns that override exclusion |

### Evaluation Rule

```text
shouldCrawl = matchesFileType
  && (matchesPatternsToKeep || !matchesPatternsToExclude)
```

**Key properties:**
- `fileTypes` is the primary filter (allowlist)
- `patternsToKeep` only overrides exclusion, does NOT force unsupported file types
- No multi-layer include/exclude semantics (single tier only)

### Chunking Strategies

Valid strategy names (from `src/engine/config.rs`):

| Strategy | File Types | Description |
|----------|------------|-------------|
| `typescript` | `.ts`, `.tsx` | AST-based semantic chunking |
| `javascript` | `.js`, `.jsx`, `.cjs`, `.mjs` | Currently skipped (returns empty) |
| `markdown` | `.md` | Heading-based chunking |
| `json` | `.json` | Currently skipped (low value for search) |
| `simpleLine` | `.txt`, `.css`, `.scss`, `.yml`, `.yaml` | Line-based chunking |

**Note:** `javascript` and `json` strategies exist but return empty chunks in current implementation. Config may specify them, but files won't be indexed until strategies are implemented.

### Pattern Matching

- Patterns use Rust glob semantics via `globset` crate
- Matching is against **repo-relative paths** (not absolute)
- Path separator is `/`
- Paths must be normalized before matching
- Matching is case-sensitive (v1)
- Invalid patterns → config validation error

### Config Discovery

Exactly one config is used. No merging. Precedence:

1. **Repo-local config**: `<repo-root>/monodex-crawl.json`
2. **User-global config**: `~/.config/monodex/crawl.json`
3. **Built-in default**: Embedded in binary (same JSON format)

### Validation

Strict validation (no silent fallback):

- Required fields must be present
- Unknown fields → error
- Incorrect types → error
- Unsupported `version` → error
- Unknown strategy names → error
- Invalid glob patterns → error

### Working Directory Mode

The same crawl config applies to both:
- Commit-based crawling (`--commit`)
- Working directory crawling (`--working-dir`)

Working directory is treated as a "degenerate commit" - same filtering rules apply.

### Example: Exclusion with Override

Given config:
```json
{
  "fileTypes": { ".ts": "typescript" },
  "patternsToExclude": ["*.test.ts"],
  "patternsToKeep": ["src/"]
}
```

| Path | Result | Reason |
|------|--------|--------|
| `src/utils.test.ts` | **Crawled** | Matches `patternsToKeep` (overrides exclude) |
| `lib/utils.test.ts` | **Skipped** | Matches `patternsToExclude`, no keep override |
| `src/utils.ts` | **Crawled** | No exclusion match |
| `lib/utils.ts` | **Crawled** | No exclusion match |

---

## Future Work

### Non-Git Catalog Types

- GitHub Issues
- Zulip Discussions
- Meeting Notes

### SQLite for Operational State

Long-term architecture: `Qdrant + SQLite`, but not required for this phase.

### Path-Based View

Query chunks by path instead of file ID:

```bash
monodex view --catalog rushstack --label main --path libraries/node-core-library/src/JsonFile.ts
```

Deferred until use cases are clearer. Primary workflow is search → view.
