# monodex Design Document

## Overview

monodex is a semantic search indexer for Rush monorepos, using Qdrant vector database with local embeddings.

---

## Development Plan

### Phase 1: Better Embedding Model
**Goal:** Switch to a code-specific model with larger context window.

1. [x] Update embedder.rs to use `jina-embeddings-v2-base-code`
2. [x] Update vector dimension from 384 to 768
3. [x] Test single chunk embedding with new model
4. [x] Commit changes

### Phase 2: Adjust Chunking for Larger Context
**Goal:** Take advantage of 8192 token budget.

1. [x] Increase `target_size` in partitioner.rs (1800 → 6000 chars)
2. [x] Verify chunks fit within token budget
3. [x] Test chunking on sample files
4. [x] Commit changes

### Phase 3: Hash-Based IDs (Superseded by Phase 6)
**Goal:** Stable IDs for search result correlation.

1. [x] Implement `compute_chunk_id(file, start_line, part) -> u64`
2. [x] Update uploader.rs to use hash IDs instead of UUIDs
3. [x] Add `part_count` field to payload
4. [x] Test ID determinism
5. [x] Commit changes

**Note:** This approach used chunk-level hashes. Phase 6 redesigns this to use file-level hashes with chunk number selectors.

### Phase 4: Performance Optimization
**Goal:** Speed up crawling with parallel processing.

1. [x] Add `rayon` dependency for thread pool
2. [x] Create multiple ONNX sessions (one per thread)
3. [x] Implement parallel chunk processing
4. [x] Configure: 4 threads × 3 intra-op threads = 12 cores
5. [x] Benchmark and verify ~12ms per embedding
6. [x] Commit changes

### Phase 5: Rebuild Database
**Goal:** Create new collection with improved schema.

1. [x] Create new Qdrant collection (768 dims for jina)
2. [x] Run full crawl with new model and chunking
3. [x] Verify crawl completes in reasonable time
4. [x] Create snapshot and check file size
5. [x] Delete old collection after verification

### Phase 6: Schema Changes (Crawl Side)
**Goal:** Update data model for file-based IDs.

1. [x] Add `compute_file_id()` to util.rs (hash of relative path)
2. [x] Add `file_id`, `relative_path`, `chunk_number`, `chunk_count` fields to Chunk struct
3. [x] Update partitioner to compute and assign chunk numbers (1-indexed, ordered by start_line)
4. [x] Update QdrantUploader to use random UUIDs for point IDs
5. [x] Update QdrantUploader to store new fields in payload

### Phase 7: Query Infrastructure
**Goal:** Build query-side support for file-based IDs.

6. [x] Implement selector parsing (`:N`, `:N-M`, `:N-end`) in main.rs
7. [x] Add `get_chunks_by_file_id()` method to QdrantUploader (filters by file_id, returns sorted by chunk_number)
8. [x] Add catalog preamble logic (collect unique catalogs, look up paths from config, show by default, omit with `--chunks-only`)

### Phase 8: CLI Commands
**Goal:** Update all user-facing commands.

9. [x] Update `view` command with new output format (header, selector support, multi-chunk)
10. [x] Add `--full-paths` flag to `view` command
11. [x] Add `--chunks-only` flag to `view` command
12. [x] Add error handling for missing chunks (`ERROR: CHUNK NOT FOUND`)
13. [x] Update `search` command output to show `<file_id>:<chunk_number>`
14. [x] ~~Update `dump-chunks` command output with file ID and chunk numbers~~ (N/A - debugging tool, not database query)
15. [x] ~~Update `audit-chunks` command output with file IDs~~ (N/A - debugging tool, not database query)

### Phase 9: Remove query Command
**Goal:** Simplify CLI by removing redundant command.

1. [x] Remove `query` command from main.rs
2. [x] Remove `query` from CLI help and documentation
3. [x] Commit changes

### Phase 10: Configurable File Exclusions
**Goal:** Move hardcoded exclusion rules to user-editable config.

1. [ ] Add `exclude` field to `CatalogConfig` struct
2. [ ] Support glob patterns for files and directories to skip
3. [ ] Migrate rules from hardcoded `config.rs` to config.jsonc
4. [ ] Update README.md with new config options
5. [ ] Commit changes

## Backlog for Future Improvements

### Critical: Early Exit on Embedding Error Skips Flush

**Issue:** The `try_for_each(...)?` pattern exits early on first embedding error without:
- Setting `stop_flag`
- Closing the channel
- Joining the uploader thread
- Flushing remaining chunks

**Impact:**
- Uploader thread may not flush remaining chunks
- Partial work lost unpredictably
- No graceful shutdown

**Fix:** Wrap the embedding loop in a closure that ensures cleanup on error:
```rust
let embed_result = all_chunks.into_par_iter()...try_for_each(...);
// Always set stop_flag and join threads, even on error
stop_flag.store(true, Ordering::Relaxed);
drop(embed_tx);
let _ = progress_thread.join();
let _ = uploader_thread.join();
embed_result?;
```

---

### Upload Failures Treated as Success

**Issue:** `upload_batch()` errors are only logged, not propagated. Failed batches are discarded (`accumulated.clear()`).

**Impact:**
- Chunks can be silently lost
- Crawl reports success despite missing data
- File completion tracking may be incorrect

**Fix Options:**
- Retry with exponential backoff
- Abort crawl on persistent failure
- Track failed batches for recovery

---

### Files Deleted Before Replacement Indexing Succeeds

**Issue:** For changed files, existing chunks are deleted immediately before re-indexing.

**Impact:**
- If chunking/embedding/upload fails, file is permanently missing from index
- No rollback or recovery mechanism

**Fix:** Consider "replace after success" pattern:
- Upload new chunks first
- Only delete old chunks after new ones are confirmed
- Or use versioned file paths to allow atomic swap

---

### Orphaned Chunks When Chunk #1 Missing

**Issue:** `get_catalog_files()` filters on `chunk_number = 1`. Files missing chunk #1 are invisible to catalog view.

**Impact:**
- Files are correctly re-crawled (good)
- But old orphaned chunks remain in Qdrant (bad)
- Accumulation of orphaned data over time

**Fix:** Periodic garbage collection:
- Scan for chunks without corresponding chunk #1
- Or use Qdrant's payload-based deletion for cleanup

---

### Unbounded Write Batching

**Issue:** The crawl command accumulates points for 60 seconds and sends one upsert with all accumulated items. There is no limit on number of points or total request size.

**Risks:**
- Can exceed Qdrant's 32 MB request size limit
- Large serialization spikes
- Increased latency and memory pressure
- Potential request rejection

**Recommended Fix:**
Introduce hard batching constraints:
- `max_points_per_batch`
- `max_estimated_bytes_per_batch`
- Optional: `max_time_window`

Flush when ANY condition is met:
- point_count >= N
- estimated_bytes >= M
- elapsed_time >= T

**Estimate size using:**
- vector length (768 × 4 bytes = 3KB)
- payload text length
- fixed overhead per item

### Payload Optimization for Scroll Operations

**Status:** Partially addressed. `get_catalog_files()` now filters on `chunk_number=1` to reduce scan volume.

**Remaining optimization:** Restrict payload fields to only those needed (`source_uri`, `content_hash`, `file_complete`). Qdrant supports selecting specific fields in scroll requests, which would avoid transferring large `text` fields.

---

## Embedding Model

### Current: jina-embeddings-v2-base-code

| Property | Value |
|----------|-------|
| Max tokens | **8192** |
| Dimensions | 768 |
| Model size | ~612MB (FP32 ONNX) |
| License | Apache 2.0 |
| Trained on | **Code + documentation** (github-code, 150M code-docstring pairs) |

**Benefits:**
- 16x larger context → most functions fit in single chunk
- 768 dimensions → more expressive embeddings
- Code-specific training → understands TypeScript, JavaScript, Python, C++, Markdown, JSON, etc.
- Docstring awareness → queries like "how to read JSON files" match `JsonFile.load()`

---

## Performance Optimization

### Key Findings from Benchmark

After extensive testing (see `../benchmark/README.md`), we determined:

| Approach | Performance | Verdict |
|----------|-------------|---------|
| ONNX CPU (12 threads) | ~16ms/embedding | ✅ **Recommended** |
| ONNX CoreML (GPU) | ~1400ms/embedding | ❌ 30x slower |
| Candle Metal | N/A | ❌ Not supported |
| Batching | 74% slower per item | ❌ Counterproductive |
| Parallel sessions | ~12ms/embedding | ✅ **Best approach** |

### Why GPU Didn't Help

1. **Embedding models are shallow** (12 layers vs 32-96+ for LLMs)
2. **Small tensor sizes** don't benefit from GPU parallelism
3. **Data transfer overhead** dominates for small operations
4. **CoreML compilation overhead** is significant

### Why Batching Hurt Performance

1. **Variable-length sequences** require padding to max length
2. **Short sequences waste compute** when padded to match longer ones
3. **Attention is O(n²)** in sequence length - padding is expensive
4. **No GPU to amortize** kernel launch overhead

### Why Not Quantized (INT8)

| Model | Size | Speed | Accuracy |
|-------|------|-------|----------|
| FP32 | 612 MB | Baseline | 100% |
| INT8 | 154 MB | 2x faster | 98.5-99.4% similarity |

**Verdict:** The 2x speedup isn't worth 1-2% accuracy loss for semantic search.

### Implementation Strategy

```rust
use rayon::prelude::*;
use std::sync::Arc;

// Configuration
const NUM_WORKERS: usize = 4;
const INTRA_THREADS: usize = 3; // 4 × 3 = 12 cores

// Each worker gets its own ONNX session
let sessions: Vec<Arc<Mutex<Session>>> = (0..NUM_WORKERS)
    .map(|_| Arc::new(Mutex::new(create_session(INTRA_THREADS)?)))
    .collect();

// Process chunks in parallel
chunks.par_iter()
    .with_max_len(1) // One chunk per task
    .enumerate()
    .for_each(|(i, chunk)| {
        let session = &sessions[i % NUM_WORKERS];
        let mut sess = session.lock().unwrap();
        let embedding = sess.encode(chunk);
        // ... store embedding
    });
```

### Expected Performance

| Machine | Strategy | Time |
|---------|----------|------|
| M3 MacBook Pro | 4 parallel sessions | ~15-20 min for full crawl |
| Any modern CPU | ONNX with threading | ~20-30 min |

---

## Chunk ID Design

### Requirements

- **File-based**: ID identifies a file, with chunk number as selector
- **Stable**: Same file path = same ID across sessions and machines
- **Human-friendly**: Easy to read, type, and correlate
- **No coordination**: No locks or counters needed

### Solution: File Hash + Chunk Number

```rust
use std::hash::{Hash, Hasher};
use twox_hash::XxHash64;

fn compute_file_id(relative_path: &str) -> u64 {
    let mut hasher = XxHash64::with_seed(0);
    relative_path.hash(&mut hasher);
    hasher.finish()
}
```

**Key insight:** The hash is based on the **relative path** from the catalog base (the `path` field in `config.jsonc`), not the full filesystem path. This ensures:
- IDs are stable across different machines/users
- Moving the repo doesn't break IDs
- IDs work regardless of where the catalog is mounted

**Relative path computation:**
- Strip the catalog's `path` prefix from the full file path
- Normalize to forward slashes (for cross-platform consistency)
- Example: If catalog path is `/Users/foo/rushstack` and file is `/Users/foo/rushstack/libraries/rush-lib/src/JsonFile.ts`, the relative path is `libraries/rush-lib/src/JsonFile.ts`

**Chunk numbering:**
- 1-indexed, ordered by `start_line` (ascending)
- Assigned after all chunks are created for a file
- Each chunk output shows its position: `(3/12)` means chunk 3 of 12 total

### Display Format

The user-facing ID format is:

```
<file_hash_hex>:<chunk_number>
```

Examples:
- `700a4ba232fe9ddc` — whole file (all chunks)
- `700a4ba232fe9ddc:3` — chunk 3 of that file
- `700a4ba232fe9ddc:2-3` — chunks 2 through 3
- `700a4ba232fe9ddc:3-end` — chunk 3 through the last chunk

**Why hex:**
- Only characters `0-9` and `a-f` - no ambiguity
- Case-insensitive (lowercase convention)
- Tokenizers handle hex strings cleanly
- When LLM outputs `700a4ba232fe9ddc:3`, it won't typo it

### Qdrant Point IDs

The Qdrant point ID is an **internal implementation detail**. We use random UUIDs for points. The mapping from user-facing ID to Qdrant points is done via payload filtering:

```
User requests: 700a4ba232fe9ddc:3
↓
Query Qdrant: filter by file_id=700a4ba232fe9ddc, chunk_number=3
↓
Return matching point(s)
```

This approach:
- Keeps Qdrant simple (no custom ID computation needed)
- Works cleanly across different source types (code, issues, chats)
- Avoids transactional complexities

---

## Chunk Structure

### Qdrant Payload Schema

```json
{
  "id": "uuid-random",
  "vector": [0.1, 0.2, ...],
  "payload": {
    "text": "export interface IExtractorConfigParameters { ... }",
    "file_id": "700a4ba232fe9ddc",
    "relative_path": "libraries/rush-lib/src/logic/pnpm/PnpmShrinkwrapFile.ts",
    "catalog": "rushstack",
    "start_line": 196,
    "end_line": 229,
    "chunk_number": 3,
    "chunk_count": 12,
    "chunk_type": "interface",
    "chunk_kind": "content",
    "symbol_name": "IExtractorConfigParameters",
    "breadcrumb": "@microsoft/rush-lib:PnpmShrinkwrapFile.ts:IExtractorConfigParameters",
    "content_hash": "sha256:abc123..."
  }
}
```

### Field Descriptions

| Field | Type | Description |
|-------|------|-------------|
| `text` | string | The actual chunk content |
| `file_id` | string | 16-char hex hash of relative path |
| `relative_path` | string | Path relative to catalog base (e.g., `libraries/rush-lib/src/...`) |
| `catalog` | string | Catalog name from config |
| `start_line` | number | First line number (1-indexed) |
| `end_line` | number | Last line number (inclusive) |
| `chunk_number` | number | Position in file (1-indexed) |
| `chunk_count` | number | Total chunks in this file |
| `chunk_type` | string | AST node type: `function`, `class`, `method`, `interface`, etc. |
| `chunk_kind` | string | Content category: `content`, `imports`, `changelog`, `config` |
| `symbol_name` | string | Primary symbol name (kept for filtering, not displayed) |
| `breadcrumb` | string | Human-readable path: `package:File.ts:Symbol` |
| `content_hash` | string | Hash of chunk text for incremental updates |

### chunk_kind Values

| Value | Description |
|-------|-------------|
| `content` | Normal code chunks: functions, classes, methods, types |
| `imports` | Import statements at top of file |
| `changelog` | CHANGELOG.md content (historical, often outdated) |
| `config` | Configuration files like package.json |

This allows search to prioritize `content` chunks while still returning other kinds if no better match exists.

---

## Query Interface

### Commands

```bash
# Search with compact blurb output
monodex search --text <query> [--limit N] [--catalog NAME]

# View chunks with selector syntax
monodex view --id <file_id>[:<selector>] [--full-paths] [--chunks-only]

# Multiple independent selectors (each --id takes one file_id with optional selector)
monodex view --id 700a4ba232fe9ddc:2-3 --id e9ddc700a4ba232f:11
```

**Note:** Each `--id` argument accepts one file_id with an optional selector. Multiple `--id` arguments can be used to request chunks from different files.

### Selector Syntax

| Selector | Meaning |
|----------|---------|
| (none) | All chunks in the file |
| `:N` | Single chunk N |
| `:N-M` | Chunks N through M (inclusive) |
| `:N-end` | Chunk N through the last chunk |

**Chunk ordering:** When multiple chunks are returned, they are sorted by `chunk_number` in ascending order.

### Output Format

#### Default Output (with catalog preamble)

```
Catalogs:
- rushstack
  Catalog path: /Users/bytedance/ai/qdrant/rushstack

700a4ba232fe9ddc:2 (2/12) @microsoft/rush-lib:PnpmShrinkwrapFile.ts:PnpmShrinkwrapFile
Source: rushstack:libraries/rush-lib/src/logic/pnpm/PnpmShrinkwrapFile.ts
Lines: 350-393
Type: class
>   /**
>    * Loads the shrinkwrap file from disk.
>    */
>   public static loadFromFile(filePath: string): PnpmShrinkwrapFile {
>     // implementation omitted
>   }

700a4ba232fe9ddc:3 (3/12) @microsoft/rush-lib:PnpmShrinkwrapFile.ts:PnpmShrinkwrapFile
Source: rushstack:libraries/rush-lib/src/logic/pnpm/PnpmShrinkwrapFile.ts
Lines: 394-462
Type: class
>   /**
>    * Clears the cache of PnpmShrinkwrapFile instances to free up memory.
>    */
>   public static clearCache(): void {
>     cacheByLockfileHash.clear();
>   }
```

#### With --full-paths

Adds a `Full path:` line showing the absolute filesystem path:

```
700a4ba232fe9ddc:2 (2/12) @microsoft/rush-lib:PnpmShrinkwrapFile.ts:PnpmShrinkwrapFile
Source: rushstack:libraries/rush-lib/src/logic/pnpm/PnpmShrinkwrapFile.ts
Full path: /Users/bytedance/ai/qdrant/rushstack/libraries/rush-lib/src/logic/pnpm/PnpmShrinkwrapFile.ts
Lines: 350-393
Type: class
>   ...
```

#### With --chunks-only

Omits the catalog preamble, showing only chunk content. Useful for scripts or when the catalog context is not needed.

**Default behavior:** The catalog preamble is shown by default when using `view` command. Use `--chunks-only` to suppress it.

#### Error Handling

When a requested chunk cannot be found, an error line is displayed in place of the chunk content. Other requested chunks are still shown:

```
700a4ba232fe9ddc:3 ERROR: CHUNK NOT FOUND
```

Example with mixed results:

```
Catalogs:
- rushstack
  Catalog path: /Users/bytedance/ai/qdrant/rushstack

700a4ba232fe9ddc:2 (2/12) @microsoft/rush-lib:PnpmShrinkwrapFile.ts:PnpmShrinkwrapFile
Source: rushstack:libraries/rush-lib/src/logic/pnpm/PnpmShrinkwrapFile.ts
Full path: /Users/bytedance/ai/qdrant/rushstack/libraries/rush-lib/src/logic/pnpm/PnpmShrinkwrapFile.ts
Lines: 350-393
Type: class
>   /**
>    * Loads the shrinkwrap file from disk.
>    */

700a4ba232fe9ddc:3 ERROR: CHUNK NOT FOUND

e9ddc700a4ba232f:11 (11/18) some-package:ExampleFile.ts:SomeClass
Source: rushstack:packages/some-package/src/ExampleFile.ts
Full path: /Users/bytedance/ai/qdrant/rushstack/packages/some-package/src/ExampleFile.ts
Lines: 820-870
Type: method
>   /**
>    * Disposes resources held by this instance.
>    */
```

### Chunk Output Fields

| Field | Description |
|-------|-------------|
| Header line | `<file_id>:<chunk_number> (<n>/<total>) <breadcrumb>` |
| Source | `<catalog>:<relative_path>` |
| Full path | (optional, with `--full-paths`) Absolute filesystem path |
| Lines | `start-end` (inclusive) |
| Type | AST node type |
| Content | Prefixed with `>` |

**Note:** `symbol_name` is kept in the database but not displayed (redundant with breadcrumb).

### Search Blurbs

Search output shows concise summaries for AI assistants:

```
700a4ba232fe9ddc:3  0.87  @microsoft/rush-lib:PnpmShrinkwrapFile.ts:clearCache
>   /**
>    * Clears the cache of PnpmShrinkwrapFile instances to free up memory.
>    */
>   public static clearCache(): void {
```

- Line 1: `<file_id>:<chunk_number>  <score>  <breadcrumb>`
- Lines 2-4: First 3 lines of code, prefixed with `>`
- Blank line between results

The `>` prefix ensures code is clearly marked as quoted content, preventing injection attacks.

---

## Chunking Analysis

### Overlap Between Chunks (Tabled)

**Industry standard (LlamaIndex):** Most semantic chunkers use overlap:
- `CodeSplitter`: 15 lines overlap
- `SentenceSplitter`: 20 characters overlap
- `TokenTextSplitter`: 20 tokens overlap

**Our current approach:** Zero overlap.

**Why we're tabling this for now:**

With our improved scope-based chunking algorithm, we now produce semantically meaningful splits at natural code boundaries (between methods, event handlers, logical sections). Given:

1. **Large chunks provide context** - 6000 chars (~120 lines) is substantial
2. **Semantic boundaries are meaningful** - we split where code logically separates
3. **Cost/benefit unclear** - storage duplication vs marginal recall improvement
4. **No evidence of problem yet** - haven't seen cases where queries fail due to boundary issues

We'll keep overlap as a potential enhancement if we encounter specific cases where it would solve a real problem, rather than implementing it speculatively.

**If needed later:**
- Apply as post-processing step (doesn't affect splitting algorithm)
- Configurable overlap (default 0, optional 10-20 lines)
- Adjust target_size to account for overlap budget

### Chunking Algorithm

**Goal:** Divide a file into chunks that fit the embedding budget, splitting only at meaningful AST boundaries.

#### Two Worlds Model

The algorithm coordinates two separate concerns:

**Chunk Land (sizing/selection):**
- The file is a sequence of line ranges (chunks)
- Can measure any chunk's size in characters
- Can split a chunk at a given line number
- Knows the budget and when we're done
- Simple bookkeeping, no AST knowledge

**AST Land (structure/meaning):**
- Recursively walks the syntax tree
- Provides **candidate split points** as line numbers
- "Meaningful" = doesn't break semantic units (e.g., between methods, not mid-function)
- No opinions about sizes, only structure

#### Coordination Algorithm

```
1. Chunk land: Start with one chunk = entire file

2. While any chunk exceeds budget:
   a. Chunk land: "Chunk X at lines [a,b] is too big"
   b. AST land: "Meaningful split points in [a,b]: [line1, line2, line3, ...]"
   c. Chunk land: Try splits, pick the one that best balances sizes
   d. Chunk land: Replace chunk X with two new chunks at the split point

3. Done - all chunks fit budget
```

#### Scope-Based Splitting

The AST traversal uses two key concepts:

**Split Scopes:** AST nodes whose direct children define split boundaries:
- `program` / `source_file` (top-level)
- `class_body`, `declaration_list`, `object_type` (type bodies)
- `statement_block`, `switch_body` (code blocks)

**Transparent Conduits:** Wrapper nodes to pass through when looking for split scopes:
- Control flow: `if_statement`, `try_statement`, loops, `switch_case`
- Declarations: `function_declaration`, `method_definition`, `arrow_function`
- Expressions: `return_statement`, `throw_statement`, `expression_statement`
- Expression wrappers: `await_expression`, `new_expression`, `arguments`, `call_expression`

**Core rule:** Choose the **shallowest split scope** that yields a usable partition.

This means:
1. Start at the shallowest scope spanning the chunk
2. If its children don't yield usable splits, descend through transparent conduits
3. Continue until finding a scope with meaningful boundaries

#### Minimum Size Constraints

To avoid pathological splits:

- **Minimum chunk size:** 20% of target (1200 chars for 6000 target)
- Both resulting chunks must meet the minimum
- Tiny nested scopes that cannot produce viable candidates (meeting `min_chunk_size`) are not considered for descent

#### Split Outcome Categories

The algorithm distinguishes three possible outcomes when attempting to split:

1. **Good AST split** (success)
   - Semantically meaningful split point found
   - Both resulting chunks respect `min_chunk_size`
   - This is the intended behavior

2. **Degraded AST split** (quality failure)
   - Semantically meaningful split point found
   - But one or both resulting chunks are below `min_chunk_size`
   - Marked in output with `:[degraded-ast-split]` breadcrumb suffix
   - Still preferable to fallback in production, but indicates partitioning difficulty

3. **Fallback split** (algorithm failure)
   - No acceptable AST split point found
   - Line-based midpoint splitting used instead
   - Marked in output with `:[fallback-split]` breadcrumb suffix
   - This is **not** a heuristic choice — it is an explicit failure mode
   - When fallback occurs, the partitioner could not find any semantic structure to use

**Design principle:** Fallback means the AST algorithm failed to produce an acceptable semantic split. A degraded AST split means the algorithm found structure, but not a high-quality partition. A good AST split means the algorithm succeeded.

#### What "Meaningful" Means

Split points are between AST siblings that don't share a semantic relationship:
- Between top-level statements (imports, classes, functions, constants)
- Between methods in a class
- Between properties in an interface
- Between statements in a function body
- Between large expression statements (e.g., event handlers)

NOT meaningful:
- Mid-expression
- Between a comment and its target code
- Inside a parameter list or type definition
- Inside tiny nested functions or callbacks

#### Wrapper Lines (Critical Detail)

When splitting a parent node into children, the parent's wrapper lines attach to the child chunks:

```
/** Class JSDoc */      // L0  ─┐
class A {               // L1   │ These attach to first method's chunk
  /** a1 JSDoc */       // L2  ─┘
  a1() {}               // L3  ← First split point after this
  /** a2 JSDoc */       // L4
  a2() {}               // L5  ← Second split point after this
  /** a3 JSDoc */       // L6
  a3() {}               // L7 ─┐
}                       // L8 ─┘─ Closing brace attaches to last method's chunk
```

If we split this class into three chunks (one per method):
- Chunk 1: L0-L3 (class JSDoc, class header, a1)
- Chunk 2: L4-L5 (a2's JSDoc, a2)
- Chunk 3: L6-L8 (a3's JSDoc, a3, closing brace)

**Key insight:** The split point is AFTER a sibling, but BEFORE the next sibling's JSDoc. The class header "comes along for the ride" with the first method.

#### Never Recombine Fragments

Once we decide to split A and B (two classes), we never recombine a fragment of A with a fragment of B. We accept smaller chunks from A rather than mixing unrelated AST branches.

Example: If A's methods are tiny but A and B are split, we keep all of A's methods together rather than combining some of A's methods with some of B's methods.

---

## Quality Gates and Investigation Workflow

The chunking system has two separate quality signals that serve different purposes.

### Two Quality Gates

**Gate 1: Correctness/Coverage**
- Question: "Can AST-based chunking handle this file without fallback?"
- Signal: **Warnings during crawl** (fallback or degraded AST splits)
- Meaning: The partitioner either failed to find AST split points (fallback) or found only poor-quality ones (degraded)
- Action: Fix the partitioner to handle this file better

**Gate 2: Quality/Optimization**
- Question: "Given AST-based chunking, are the chunk boundaries good?"
- Signal: **Scores in audit-chunks**
- Meaning: The partitioner found split points, but they may be suboptimal
- Action: Tune heuristics for better boundaries

### Why Scores Need AST-Only Mode

When fallback splitting is used, it cuts text at line midpoints, often producing chunks that happen to be near the target size. This can inflate quality scores even though the AST-based chunker failed.

To make scores meaningful, `audit-chunks` and `dump-chunks` (by default) disable fallback:
- Oversized chunks remain oversized
- Score reflects AST-only partitioning quality
- High score = "AST chunking worked well"

### Investigation Workflow

1. **Run crawl** over the entire corpus
2. **Notice chunking warnings** - these are Gate 1 defects
3. **Use dump-chunks** (AST-only mode) to see oversized chunks
4. **Fix the partitioner** to handle those files
5. **Re-crawl** to confirm warnings are gone
6. **Run audit-chunks** to find suboptimal but AST-valid chunking (Gate 2)
7. **Use dump-chunks --debug** to inspect why split points were chosen

### Breadcrumb Quality Markers

When chunking produces non-ideal outcomes, breadcrumbs include markers for visibility:

**Fallback split** (algorithm failure):
```
@microsoft/rush-lib:WorkspaceInstallManager.ts:prepareCommonTempAsync:[fallback-split]
```

**Degraded AST split** (quality failure):
```
@rushstack/node-core-library:IPackageJson.ts:[degraded-ast-split]
```

These markers are **per-chunk**, not file-level. Only chunks actually created by the indicated method get marked.

**Important:** Fallback is a failure mode, not part of the correctly functioning heuristic. When fallback occurs, it means the partitioner could not find any semantic structure to use. The fallback provides damage control for production, but the warning should trigger investigation.

### Sticky Warning Recrawl

Files with chunking warnings are tracked in `.monodex-warnings-<catalog>.json`. This ensures:
- Files with warnings are re-crawled even if content hash unchanged
- After fixing the partitioner, re-crawl verifies the fix
- Warning state is separate from Qdrant (operational ledger, not indexed content)

---

## Schema Migration

| Aspect | Old | New |
|--------|-----|-----|
| Model | bge-small-en-v1.5 | jina-embeddings-v2-base-code |
| Vector dims | 384 | 768 |
| ID type | UUID string | file hash + chunk selector |
| Chunk target | 1800 chars | 6000 chars |
| Paths | Full paths | Relative paths from catalog base |

**Migration:** Delete old collection and re-crawl with new schema.

---

## Schema Evolution: Multi-Source Support

### Future Content Types

Beyond source code, we will crawl:
- **GitHub Issues**: Each comment as a chunk
- **Zulip Discussions**: Each message as a chunk
- **Rush Hour Meeting Notes**: Sections/paragraphs as chunks

### Design Principle: Tagged Union (Apples and Oranges in Separate Boxes)

Rather than over-abstracting into a "one size fits all" schema, we use a type discriminator (`source_type`) and keep type-specific fields separate. Code that handles query results dispatches on `source_type` and knows exactly which fields to expect.

### Schema Structure

```rust
// Conceptual - actual implementation may vary
enum SourceType {
    Code,       // source code files
    Issue,      // GitHub issues
    Discussion, // Zulip threads
    Document,   // Meeting notes, etc.
}

struct CodeChunk {
    // Common fields
    text: String,
    content_hash: String,
    catalog: String,
    breadcrumb: String,
    chunk_type: String,

    // Code-specific
    file_id: String,
    relative_path: String,
    start_line: usize,
    end_line: usize,
    chunk_number: usize,
    chunk_count: usize,
    symbol_name: Option<String>,
}

struct IssueChunk {
    // Common fields
    text: String,
    content_hash: String,
    catalog: String,
    breadcrumb: String,
    chunk_type: String,

    // Issue-specific
    repo: String,
    issue_number: u64,
    comment_index: Option<usize>,  // None = original post
    author: String,
}
```

### Qdrant Payload

Stored as JSON with a `source_type` discriminator:

```json
// Code chunk
{
  "source_type": "code",
  "text": "fn load() { ... }",
  "content_hash": "sha256:abc123",
  "catalog": "rushstack",
  "breadcrumb": "@rushstack/node-core-library:JsonFile.ts:load",
  "chunk_type": "function",
  "file_id": "700a4ba232fe9ddc",
  "relative_path": "libraries/rush-lib/src/JsonFile.ts",
  "start_line": 10,
  "end_line": 50,
  "chunk_number": 3,
  "chunk_count": 7,
  "symbol_name": "load"
}

// Issue chunk
{
  "source_type": "issue",
  "text": "When I try to...",
  "content_hash": "sha256:def456",
  "catalog": "rushstack-issues",
  "breadcrumb": "rushstack/rush-stack#123 > comment-2",
  "chunk_type": "issue-comment",
  "repo": "rushstack/rush-stack",
  "issue_number": 123,
  "comment_index": 2,
  "author": "someuser"
}
```

### Query Filtering

```bash
# Search only code
monodex search --text "json parsing" --type code

# Search only issues
monodex search --text "build error" --type issue

# Search everything
monodex search --text "api design"
```

### Implementation Approach

1. Add `source_type: String` field now (default: `"code"`)
2. Make type-specific fields `Option<T>` where needed
3. When adding new source types, extend payload with type-specific fields
4. Query handler dispatches on `source_type` to render appropriately
