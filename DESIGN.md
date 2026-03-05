# rush-qdrant Design Document

## Overview

rush-qdrant is a semantic search indexer for Rush monorepos, using Qdrant vector database with local embeddings. This document captures design decisions for both the indexing pipeline and query interface.

---

## Development Plan

### Phase 1: Better Embedding Model
**Goal:** Switch to a code-specific model with larger context window to reduce chunk splitting.

1. [ ] Research and select new model (jina-embeddings-v2-base-code: 8192 tokens, code-trained)
2. [ ] Add optional Metal/CUDA features to Cargo.toml
3. [ ] Implement device selection logic (Metal → CUDA → CPU fallback)
4. [ ] Update embedder.rs to support new model
5. [ ] Test single chunk embedding with new model
6. [ ] Commit changes

### Phase 2: Adjust Chunking for Larger Context
**Goal:** Take advantage of larger token budget.

1. [ ] Increase `target_size` in partitioner.rs (e.g., 1800 → 6000 chars)
2. [ ] Verify chunks fit within new token budget
3. [ ] Test chunking on sample files
4. [ ] Commit changes

### Phase 3: Schema Changes
**Goal:** Implement stable hash-based IDs.

1. [ ] Implement `compute_chunk_id(file, start_line, part) -> u64`
2. [ ] Update uploader.rs to use hash IDs instead of UUIDs
3. [ ] Add `part_count` field to payload (to track siblings)
4. [ ] Test ID computation is deterministic
5. [ ] Commit changes

### Phase 4: Performance Optimization
**Goal:** Dramatically speed up crawling.

1. [ ] Implement GPU device selection (Metal/CUDA/CPU)
2. [ ] Add rayon parallel processing for CPU fallback
3. [ ] Test single chunk with GPU acceleration
4. [ ] Test single chunk with CPU parallelism
5. [ ] Benchmark improvements
6. [ ] Commit changes

### Phase 5: Rebuild Database
**Goal:** Create new collection with improved settings.

1. [ ] Create new Qdrant collection with correct vector size (768 dims for jina)
2. [ ] Run full crawl with new model and chunking
3. [ ] Verify crawl completes in reasonable time
4. [ ] Create snapshot and check file size
5. [ ] Compare results with old collection

### Phase 6: Query Interface Improvements
**Goal:** Better search experience for AI assistants.

1. [ ] Implement `search` command with blurb output
2. [ ] Implement `get` command for full chunks
3. [ ] Implement `expand` command for context
4. [ ] Implement `siblings` command
5. [ ] Implement `cat` command for full files
6. [ ] Add filtering (`--type`, `--symbol`, `--path`, `--min-score`)
7. [ ] Add `--json` output format
8. [ ] Commit changes

---

## Embedding Model

### Current Model: BAAI/bge-small-en-v1.5

| Property | Value |
|----------|-------|
| Max tokens | 512 |
| Dimensions | 384 |
| Model size | ~33MB |
| License | MIT |
| Trained on | English text |

**Problems:**
- 512 tokens (~2000 chars) causes excessive chunk splitting
- Functions get cut in half, losing semantic coherence
- Not specifically trained on code

### Recommended: jina-embeddings-v2-base-code

| Property | Value |
|----------|-------|
| Max tokens | **8192** |
| Dimensions | 768 |
| Model size | ~161M parameters |
| License | Apache 2.0 |
| Trained on | **Code + documentation** (github-code, 150M code-docstring pairs) |

**Benefits:**
- 16x larger context → most functions fit in single chunk
- 768 dimensions → more expressive embeddings
- **Code-specific training** → understands TypeScript, JavaScript, Python, C++, Markdown, JSON, etc.
- **Docstring awareness** → trained on code-documentation pairs

### Supported Programming Languages

The model was trained on 30+ programming languages including:
- TypeScript ✅
- JavaScript ✅
- Python
- C / C++
- Java
- Go
- Rust
- Markdown ✅
- JSON ✅
- YAML
- And 20+ more

### Why Code-Specific Models Matter

General text embedding models (like bge-small-en) treat code as English prose:
- Variable names like `pnpmfileRunner` get tokenized as nonsense
- Code structure (braces, indentation) carries no semantic meaning
- Docstrings and comments get same weight as code logic

Code-specific models (like jina-base-code):
- Understand identifier semantics (`transformPackageAsync` ≈ "transform package async")
- Learn code patterns (function signatures, class structures)
- Trained on docstring↔code pairs, so queries like "how to read JSON files" match `JsonFile.load()`

### Alternative Options

| Model | Tokens | Dims | Size | Trained On | License |
|-------|--------|------|------|------------|---------|
| **jina-base-code** ✅ | 8192 | 768 | 161M | Code + docs | Apache 2.0 |
| jina-base-en | 8192 | 768 | 137M | English text | Apache 2.0 |
| bge-m3 | 8192 | 1024 | 568M | Multilingual | MIT |
| nomic-embed-text-v1.5 | 8192 | 768 | 274M | English text | Apache 2.0 |
| voyage-code-2 | 16000 | 1536 | — | Code | Proprietary |

---

## Chunk ID Design

### Requirements
- **Stable**: Same code location = same ID across queries and sessions
- **Short**: Display-friendly, ~8 characters
- **No coordination**: Work with parallel indexing, no locks/counters needed
- **Support multiple object types**: Code chunks, GitHub issues, etc.

### Solution: Hash-Based IDs

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

### Display Format

```
u64: 18472938475621023485
display: "#100c8f3"  (first 8 hex chars, prefixed with #)
```

### Collision Tolerance

Hash collisions are acceptable because:
- IDs are for correlating search results, not primary keys
- A collision only matters if two colliding items appear in the SAME query result
- With 64-bit hash, practical collisions are vanishingly rare
- Even if collision occurs, user can disambiguate via breadcrumb/file info

---

## Chunk Structure

Each chunk in Qdrant has:

```json
{
  "id": 18472938475621023485,  // u64 hash (was UUID string)
  "vector": [0.1, 0.2, ...],   // 768 dimensions (was 384)
  "payload": {
    "text": "export interface IExtractorConfigParameters { ... }",
    "file": "/path/to/rushstack/apps/api-extractor/src/api/ExtractorConfig.ts",
    "catalog": "rushstack",
    "content_hash": "sha256:5a7bf235...",
    "start_line": 196,
    "end_line": 229,
    "symbol_name": "IExtractorConfigParameters",
    "chunk_type": "interface",
    "breadcrumb": "@microsoft/api-extractor:ExtractorConfig.ts:IExtractorConfigParameters",
    "part_number": 0,           // NEW: 0-indexed part number
    "part_count": 1             // NEW: total parts (for siblings)
  }
}
```

### Key Fields

| Field | Description | Used For |
|-------|-------------|----------|
| `text` | Full chunk content | Display, embedding |
| `file` | Absolute file path | Navigation, ID computation |
| `catalog` | Source catalog name | Filtering |
| `start_line` / `end_line` | Line range | ID computation, navigation |
| `symbol_name` | Function/class/interface name | Display, filtering |
| `chunk_type` | function, class, interface, code, markdown, text | Filtering, display |
| `breadcrumb` | Package:File:Symbol path | Human-readable location |
| `content_hash` | SHA256 of chunk text | Change detection |
| `part_number` | Index of this part (0-based) | ID computation, sibling finding |
| `part_count` | Total parts for this chunk | Sibling finding |

---

## Performance Optimization

### Problem: CPU-Only Embeddings Are Slow

Current crawl takes **hours** because:
- Model runs on CPU only (`Device::Cpu`)
- M3 MacBook Pro has powerful GPU/Neural Engine not being used
- ~350-500ms per chunk instead of ~5-20ms

### Solution: GPU Acceleration with Fallbacks

```rust
fn get_best_device() -> Result<Device> {
    // Try Metal (Apple Silicon)
    #[cfg(feature = "metal")]
    if let Ok(device) = Device::new_metal(0) {
        println!("Using Metal acceleration");
        return Ok(device);
    }
    
    // Try CUDA (NVIDIA)
    #[cfg(feature = "cuda")]
    if let Ok(device) = Device::new_cuda(0) {
        println!("Using CUDA acceleration");
        return Ok(device);
    }
    
    // Fall back to CPU
    println!("Using CPU (no GPU acceleration)");
    Ok(Device::Cpu)
}
```

### Cargo.toml Features

```toml
[features]
default = []
metal = ["candle-core/metal"]
cuda = ["candle-core/cuda"]
```

### Parallel CPU Fallback

For machines without GPU, use rayon for parallel processing:

```rust
files.par_chunks(chunk_size).for_each(|files| {
    let generator = EmbeddingGenerator::new_on_device(MODEL_ID, &device);
    // process files...
});
```

### Expected Performance

| Machine | Strategy | Expected Time |
|---------|----------|---------------|
| M3 MacBook Pro | Metal | ~5-15 min |
| CI with NVIDIA | CUDA | ~5-15 min |
| CI CPU-only | Rayon (4-8 threads) | ~30-45 min |
| CI CPU-only (current) | Single-threaded | 2-3 hours |

---

## Query Interface Design

### Core Concept: Two-Phase Search

**Phase 1: Search → Get Blurbs**
Return concise, Google-style result summaries with stable IDs.

**Phase 2: Get → Full Chunks**
Retrieve complete chunks by ID. Expand context. Navigate to siblings or full file.

This pattern minimizes token usage: explore with small blurbs, then fetch only what's needed.

### Commands

#### `rush-qdrant search`

```bash
rush-qdrant search --text <query>
  [--limit N]           # Max results (default: 20)
  [--catalog NAME]      # Filter by catalog
  [--path GLOB]         # Filter by file path glob
  [--type TYPE]         # Filter by chunk type
  [--symbol NAME]       # Filter by symbol name
  [--min-score N]       # Minimum relevance score
```

#### `rush-qdrant get`

```bash
rush-qdrant get --id <id1,id2,id3...>
```

#### `rush-qdrant expand`

```bash
rush-qdrant expand --id <id> [--context N]
```

#### `rush-qdrant siblings`

```bash
rush-qdrant siblings --id <id>
```

#### `rush-qdrant cat`

```bash
rush-qdrant cat --id <id>
rush-qdrant cat --file <path>
```

---

## Schema Migration Notes

When switching from bge-small-en-v1.5 to jina-embeddings-v2-base-code:

| Aspect | Old | New |
|--------|-----|-----|
| Model | bge-small-en-v1.5 | jina-embeddings-v2-base-code |
| Vector dimensions | 384 | 768 |
| ID type | UUID string | u64 hash |
| Chunk target | 1800 chars | 6000 chars |
| New payload fields | — | `part_number`, `part_count` |

**Migration requires:**
1. Create new Qdrant collection (different vector size)
2. Re-crawl all catalogs
3. IDs will be stable (hash-based), so references remain valid
4. Old collection can be deleted after verification

---

## Open Questions

1. **Chunk target size**: What's optimal for 8192 token budget? 6000 chars? 8000 chars?

2. **GPU memory**: Can we share model across threads on GPU, or need separate copies?

3. **Preview line count**: How many lines in blurbs? 1 line (JSDoc) or 2-3?

4. **Batch size**: Should we adjust BATCH_SIZE for larger chunks?
