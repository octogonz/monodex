# rush-qdrant Design Document

## Overview

rush-qdrant is a semantic search indexer for Rush monorepos, using Qdrant vector database with local embeddings.

---

## Development Plan

### Phase 1: Better Embedding Model
**Goal:** Switch to a code-specific model with larger context window.

1. [ ] Add optional Metal/CUDA features to Cargo.toml
2. [ ] Implement device selection logic (Metal → CUDA → CPU fallback)
3. [ ] Update embedder.rs to use jina-embeddings-v2-base-code
4. [ ] Test single chunk embedding with new model
5. [ ] Commit changes

### Phase 2: Adjust Chunking for Larger Context
**Goal:** Take advantage of 8192 token budget.

1. [ ] Increase `target_size` in partitioner.rs (1800 → 6000 chars)
2. [ ] Verify chunks fit within token budget
3. [ ] Test chunking on sample files
4. [ ] Commit changes

### Phase 3: Hash-Based IDs
**Goal:** Stable IDs for search result correlation.

1. [ ] Implement `compute_chunk_id(file, start_line, part) -> u64`
2. [ ] Update uploader.rs to use hash IDs instead of UUIDs
3. [ ] Add `part_count` field to payload
4. [ ] Test ID determinism
5. [ ] Commit changes

### Phase 4: Performance Optimization
**Goal:** Speed up crawling from hours to minutes.

1. [ ] Implement GPU device selection (Metal/CUDA/CPU)
2. [ ] Add rayon parallel processing for CPU fallback
3. [ ] Benchmark improvements
4. [ ] Commit changes

### Phase 5: Rebuild Database
**Goal:** Create new collection with improved schema.

1. [ ] Create new Qdrant collection (768 dims for jina)
2. [ ] Run full crawl with new model and chunking
3. [ ] Verify crawl completes in reasonable time
4. [ ] Create snapshot and check file size
5. [ ] Delete old collection after verification

### Phase 6: Query Interface
**Goal:** Better search for AI assistants.

1. [ ] Implement `search` command with blurb output
2. [ ] Implement `get` command for full chunks
3. [ ] Implement `expand` command (surrounding context)
4. [ ] Implement `siblings` command (other parts of split chunk)
5. [ ] Implement `cat` command for full files
6. [ ] Add filtering (`--type`, `--symbol`, `--path`, `--min-score`)
7. [ ] Add `--json` output format
8. [ ] Commit changes

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
| Model size | ~161M parameters |
| License | Apache 2.0 |
| Trained on | **Code + documentation** (github-code, 150M code-docstring pairs) |

**Benefits:**
- 16x larger context → most functions fit in single chunk
- 768 dimensions → more expressive embeddings
- Code-specific training → understands TypeScript, JavaScript, Python, C++, Markdown, JSON, etc.
- Docstring awareness → queries like "how to read JSON files" match `JsonFile.load()`

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

The u64 hash is stored as the Qdrant point ID. For display, use **8-character lowercase hex**:

```
u64 hash: 18472938475621023485
display:  "#1a2b3c4d"
```

**Why hex instead of base32/base62:**
- Only characters `0-9` and `a-f` - no ambiguity
- Case-insensitive (lowercase convention)
- Tokenizers handle hex strings cleanly
- When LLM outputs `#1a2b3c4d`, it won't typo it

Implementation:
```rust
fn display_id(hash: u64) -> String {
    format!("#{:08x}", (hash >> 32) & 0xFFFFFFFF)
}
```

### Collision Tolerance

With 32 bits (4 billion values), collisions are astronomically rare for a single repo. Even if one occurs, disambiguate via breadcrumb/file info.

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

## Performance

### Problem: CPU-Only Is Slow

Current crawl takes **hours**:
- Model runs on CPU only (`Device::Cpu`)
- ~350-500ms per chunk vs ~5-20ms with GPU

### Solution: GPU with CPU Fallback

```rust
fn get_best_device() -> Result<Device> {
    #[cfg(feature = "metal")]
    if let Ok(device) = Device::new_metal(0) {
        return Ok(device);
    }
    
    #[cfg(feature = "cuda")]
    if let Ok(device) = Device::new_cuda(0) {
        return Ok(device);
    }
    
    Ok(Device::Cpu)
}
```

### Expected Times

| Machine | Strategy | Time |
|---------|----------|------|
| M3 MacBook Pro | Metal | ~5-15 min |
| CI with NVIDIA | CUDA | ~5-15 min |
| CI CPU-only | Rayon (parallel) | ~30-45 min |
| CI CPU-only (current) | Single-threaded | 2-3 hours |

---

## Query Interface

### Two-Phase Search

1. **Search → Blurbs**: Concise summaries with stable IDs
2. **Get → Full Chunks**: Retrieve by ID, expand context, navigate siblings

### Commands

```bash
rush-qdrant search --text <query> [--limit N] [--catalog NAME] [--path GLOB] [--type TYPE]
rush-qdrant get --id <id1,id2,id3...>
rush-qdrant expand --id <id> [--context N]
rush-qdrant siblings --id <id>
rush-qdrant cat --id <id>
rush-qdrant cat --file <path>
```

---

## Schema Migration

| Aspect | Old | New |
|--------|-----|-----|
| Model | bge-small-en-v1.5 | jina-embeddings-v2-base-code |
| Vector dims | 384 | 768 |
| ID type | UUID string | u64 hash |
| Chunk target | 1800 chars | 6000 chars |
| New fields | — | `part_number`, `part_count` |

**Migration:** Create new collection (different vector size), re-crawl, delete old after verification.
