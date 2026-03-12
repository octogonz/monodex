# rush-qdrant Design Document

## Overview

rush-qdrant is a semantic search indexer for Rush monorepos, using Qdrant vector database with local embeddings.

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

### Phase 3: Hash-Based IDs
**Goal:** Stable IDs for search result correlation.

1. [x] Implement `compute_chunk_id(file, start_line, part) -> u64`
2. [x] Update uploader.rs to use hash IDs instead of UUIDs
3. [x] Add `part_count` field to payload
4. [x] Test ID determinism
5. [x] Commit changes

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

### Phase 6: Query Interface
**Goal:** Better search for AI assistants.

1. [x] Implement `search` command with blurb output
2. [x] Implement `view` command for full chunks (renamed from `get`)
3. [ ] Implement `expand` command (neighboring chunks)
4. [ ] Implement `siblings` command (other parts of split chunk)
5. [ ] Implement `cat` command for full files
6. [ ] Add filtering (`--type`, `--symbol`, `--path`, `--min-score`)

### Phase 7: Improved Chunking
**Goal:** Fix gaps and add overlap for better semantic matching.

1. [ ] Investigate missing text at AST boundaries
2. [ ] Implement chunk overlap (following LlamaIndex conventions)
3. [ ] Ensure imports are captured as separate chunks
4. [ ] Ensure file-level constants and comments are captured
5. [ ] Rebuild database with improved chunking
6. [ ] Implement `expand` command using new neighboring chunk data

---

## Embedding Model

### Current: BAAI/bge-small-en-v1.5

| Property | Value |
|----------|-------|
| Max tokens | 512 |
| Dimensions | 384 |
| Model size | ~33MB |
| Trained on | English text |

**Problems:**
- 512 tokens (~2000 chars) causes excessive chunk splitting
- Functions get cut in half, losing semantic coherence
- Not trained on code

### Target: jina-embeddings-v2-base-code

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
- **Stable**: Same code location = same ID across sessions
- **Short**: ~8 characters for display
- **No coordination**: No locks or counters needed

### Solution: Hash-Based u64

```rust
use std::hash::{Hash, Hasher};
use twox_hash::XxHash64;

fn compute_chunk_id(file: &str, start_line: usize, part: usize) -> u64 {
    let mut hasher = XxHash64::with_seed(0);
    file.hash(&mut hasher);
    start_line.hash(&mut hasher);
    part.hash(&mut hasher);
    hasher.finish()
}
```

### Display Format: Hex

The u64 hash is stored as the Qdrant point ID. For display, use **16-character lowercase hex**:

```
u64 hash: 18472938475621023485
display:  "#30440fb2ecd5fa62"
```

**Why hex instead of base32/base62:**
- Only characters `0-9` and `a-f` - no ambiguity
- Case-insensitive (lowercase convention)
- Tokenizers handle hex strings cleanly
- When LLM outputs `#30440fb2ecd5fa62`, it won't typo it

**Why 16 chars (full 64-bit) instead of 8 chars (32-bit):**
- Need full hash to retrieve chunks by ID
- 16 chars is still short enough for display
- No collision risk even across multiple repos

### Collision Tolerance

With 64 bits, collisions are astronomically rare. Even if one occurs, disambiguate via breadcrumb/file info.

---

## Chunk Structure

```json
{
  "id": 18472938475621023485,
  "vector": [0.1, 0.2, ...],
  "payload": {
    "text": "export interface IExtractorConfigParameters { ... }",
    "file": "/path/to/ExtractorConfig.ts",
    "catalog": "rushstack",
    "start_line": 196,
    "end_line": 229,
    "symbol_name": "IExtractorConfigParameters",
    "chunk_type": "interface",
    "breadcrumb": "@microsoft/api-extractor:ExtractorConfig.ts:IExtractorConfigParameters",
    "part_number": 0,
    "part_count": 1
  }
}
```

---

## Query Interface

### Two-Phase Search

1. **Search → Blurbs**: Concise summaries with stable IDs
2. **View → Full Chunks**: Retrieve by ID for complete content

### Commands

```bash
# Search with compact blurb output (for AI assistants)
rush-qdrant search --text <query> [--limit N] [--catalog NAME]

# View full chunks by ID (comma-separated for multiple)
rush-qdrant view --id <id1,id2,id3...>

# Expand to see neighboring chunks (future)
rush-qdrant expand --id <id> [--context N]

# Other planned commands
rush-qdrant siblings --id <id>
rush-qdrant cat --id <id>
rush-qdrant cat --file <path>
```

### Output Format

**Search blurbs** are designed for AI assistants:
- Line 1: `#id  score  breadcrumb`
- Lines 2-4: First 3 lines of code, prefixed with `>` (quoted)
- Blank line between results

The `>` prefix ensures code is clearly marked as quoted content, preventing injection attacks where code could be misinterpreted as instructions.

**View output** shows full chunk content:
- Header: ID, breadcrumb, source file, lines, type, symbol
- Full text quoted with `>` prefix

---

## Chunking Analysis

### Current Issues

#### Issue 1: No Overlap Between Chunks

**Industry standard (LlamaIndex):** Most semantic chunkers use overlap:
- `CodeSplitter`: 15 lines overlap
- `SentenceSplitter`: 20 characters overlap
- `TokenTextSplitter`: 20 tokens overlap

**Our implementation:** Zero overlap. This means:
- Concepts spanning chunk boundaries may be harder to find
- Boundary context is lost
- Semantic matching may miss relevant matches

**Planned fix:** Add configurable overlap (default 10-15 lines for code).

#### Issue 2: Missing Text at AST Boundaries

Analysis of `JsonFile.ts` revealed gaps in coverage:

| Gap Lines | Content | Importance |
|-----------|---------|------------|
| 1-7 | Copyright header, imports | **High** - imports show dependencies |
| 9-41 | JSDoc for `JsonObject`, `JsonNull`, `JsonSyntax` | **High** - type documentation |
| 199-206 | `const DEFAULT_ENCODING`, `export class JsonFile {` | **High** - class declaration |
| 516-517 | Comment before private method | Medium - code organization |
| 538-540 | Comment explaining `_formatKeyPath` | **High** - algorithm explanation |

**Root cause:** The partitioner only creates chunks for "meaningful nodes" (functions, methods, classes, interfaces) >= 50 chars. It misses:
- Import statements
- Standalone JSDoc/TSDoc comments
- Type aliases with documentation
- Constants defined outside functions
- Class declarations when the body is split into methods

**Planned fix:** 
1. Create separate chunks for imports sections
2. Create chunks for file-level constants and type definitions
3. Attach orphan comments to nearby code or create standalone chunks

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
- Tiny nested scopes (< 30 lines) are not considered as split candidates
- Large expression statements (> 500 bytes) are treated as meaningful boundaries

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

### Implementation Plan

#### Step 1: Add `chunk_kind` field to schema

Add to `PointPayload` in `uploader.rs`:
```rust
pub chunk_kind: String,
```

**Values:**
- `"content"` (default) - Normal code chunks: functions, classes, methods, types
- `"imports"` - Import statements at top of file (lower search relevance)
- `"changelog"` - CHANGELOG.md content (historical, often outdated)
- `"config"` - Configuration files like package.json (structural, less searchable)

This allows search to prioritize `"content"` chunks while still returning other kinds if no better match exists.

#### Step 2: Fix missing text (imports, small types)

**Changes to `partitioner.rs`:**

1. **Extract imports chunk first:**
   - Before main partition loop, walk AST for `import_statement` nodes at depth 0
   - Combine into one chunk with `chunk_kind: "imports"`
   - Breadcrumb: `package:File.ts:imports`

2. **Investigate why small type aliases are skipped:**
   - Types like `JsonObject`, `JsonNull` have JSDoc but don't appear in chunks
   - May be due to AST traversal issue, not the 50-char minimum
   - The 50-char minimum is about avoiding garbage chunks, not skipping meaningful content
   - Investigate during implementation

#### Step 3: Add overlap

**Constants:**
```rust
const OVERLAP_LINES: usize = 18;  // ~15% of 120-line average chunk
const TARGET_SIZE_WITH_OVERLAP: usize = 5100;  // leaves ~900 chars for overlap
```

**Algorithm:**
1. After partitioning, for each chunk:
   - Extend `start_line` up by `OVERLAP_LINES / 2`
   - Extend `end_line` down by `OVERLAP_LINES / 2`
   - Clamp to file boundaries
2. When calculating if chunk fits budget: `text.len() + estimated_overlap_len`

**Testing:**
- Use "tiny" catalog or define a single-project catalog (e.g., node-core-library)
- Avoid full rushstack crawl during testing (can take an hour)

### Chunk Structure

```json
{
  "id": 18472938475621023485,
  "vector": [0.1, 0.2, ...],
  "payload": {
    "text": "export interface IExtractorConfigParameters { ... }",
    "source_uri": "/path/to/ExtractorConfig.ts",
    "source_type": "code",
    "catalog": "rushstack",
    "content_hash": "sha256:abc123...",
    "start_line": 196,
    "end_line": 229,
    "symbol_name": "IExtractorConfigParameters",
    "chunk_type": "interface",
    "chunk_kind": "content",
    "breadcrumb": "@microsoft/api-extractor:ExtractorConfig.ts:IExtractorConfigParameters"
  }
}
```

**Note:** part_number and part_count were planned but not currently implemented. Chunks that exceed target size are split and marked in the breadcrumb as "(part 1/N)".

---

## Schema Migration

| Aspect | Old | New |
|--------|-----|-----|
| Model | bge-small-en-v1.5 | jina-embeddings-v2-base-code |
| Vector dims | 384 | 768 |
| ID type | UUID string | u64 hash |
| Chunk target | 1800 chars | 6000 chars |

**Migration:** Create new collection (different vector size), re-crawl, delete old after verification.

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
    file_path: String,
    start_line: usize,
    end_line: usize,
    symbol_name: Option<String>,
    part_number: usize,
    part_count: usize,
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
  "file_path": "src/JsonFile.ts",
  "start_line": 10,
  "end_line": 50,
  "symbol_name": "load",
  "part_number": 0,
  "part_count": 1
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
rush-qdrant search --text "json parsing" --type code

# Search only issues  
rush-qdrant search --text "build error" --type issue

# Search everything
rush-qdrant search --text "api design"
```

### Implementation Approach

1. Add `source_type: String` field now (default: `"code"`)
2. Make type-specific fields `Option<T>` where needed
3. Rename `file` to `source_uri` for generality
4. When adding new source types, extend payload with type-specific fields
5. Query handler dispatches on `source_type` to render appropriately


