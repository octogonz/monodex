# Rush Monodex

**Semantic search indexer for Rush monorepos using Qdrant vector database**

## Overview

`monodex` is a production-quality CLI tool that indexes Rush monorepo source code and documentation into a Qdrant vector database for high-quality semantic search.

### Features

- **AST-based chunking**: Tree-sitter powered intelligent splitting for TypeScript/TSX files
- **Breadcrumb context**: Full symbol paths like `@rushstack/node-core-library:JsonFile.ts:JsonFile.load`
- **Oversized chunk handling**: Functions split at natural AST boundaries (statement blocks, if/else, try/catch)
- **Local embeddings**: Uses jina-embeddings-v2-base-code with ONNX Runtime (no external APIs)
- **Qdrant integration**: Direct batch uploads to Qdrant vector database
- **Incremental sync**: Content-hash based change detection for fast re-indexing
- **Rush-optimized**: Smart exclusion rules for Rush monorepo patterns

## Agent Usage Guide

This tool is designed for AI assistants. The indexed database provides a complete, internally consistent snapshot of the codebase as it existed at crawl time — independent of any local file changes, branches, or whether the repo is even cloned. This makes it more than a replacement for grep; it can be the primary way an agent learns about a codebase.

**Typical workflow:**

1. **Start with semantic search** to find relevant code:
   ```bash
   monodex search --text "how does rush handle pnpm shrinkwrap files"
   ```

2. **View full chunks** using the `file_id:chunk_number` from search results:
   ```bash
   monodex view --id 700a4ba232fe9ddc:3
   ```

3. **Get surrounding context** by viewing adjacent chunks:
   ```bash
   monodex view --id 700a4ba232fe9ddc:2-4
   ```

4. **Use `--full-paths`** when you need the actual file location on disk:
   ```bash
   monodex view --id 700a4ba232fe9ddc:3 --full-paths
   ```

**Output format:** Search results prefix code lines with `>`, making them easy to distinguish from your own output and preventing injection attacks.

## Installation

```bash
# Build from source
cargo build --release

# Binary will be at ./target/release/monodex
```

## Usage

### Configuration

Create `~/.config/monodex/config.jsonc`:

```json
{
  "qdrant": {
    "url": "http://localhost:6333",
    "collection": "rushstack"
  },
  "catalogs": {
    "rushstack": {
      "type": "monorepo",
      "path": "/path/to/rushstack"
    }
  }
}
```

**Fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `qdrant.url` | No | Qdrant server URL (default: `http://localhost:6333`) |
| `qdrant.collection` | Yes | Qdrant collection name |
| `catalogs.<name>.type` | Yes | Catalog type: `"monorepo"` or `"folder"` |
| `catalogs.<name>.path` | Yes | Absolute path to the repository root |

**Catalog types:**

- **`monorepo`**: Walks upward to find the nearest `package.json` for package name resolution. Breadcrumbs show `@scope/package-name:File.ts:Symbol`.
- **`folder`**: Uses the parent folder name as the package identifier. Breadcrumbs show `folder-name:File.ts:Symbol`.

### Index a repository

```bash
# Index using config file
monodex crawl --catalog rushstack

# With custom config path
monodex --config /path/to/config.jsonc crawl --catalog rushstack
```

**Incremental sync:** The crawl is incremental — unchanged files are skipped. You can safely CTRL+C and resume later. Files with chunking warnings are always re-crawled unless `--incremental-warnings` is set.

**Warning state** is persisted in `~/.config/monodex/warnings-<catalog>.json`.

### Search the database

```bash
# Semantic search with compact blurb output (for AI assistants)
monodex search --text "how to read JSON files"

# With catalog filter and limit
monodex search --text "API Extractor" --catalog rushstack --limit 10
```

### View full chunks

```bash
# View all chunks in a file
monodex view --id 30440fb2ecd5fa62

# View a specific chunk by number
monodex view --id 30440fb2ecd5fa62:3

# View a range of chunks
monodex view --id 30440fb2ecd5fa62:2-4

# View from chunk 3 to the end
monodex view --id 30440fb2ecd5fa62:3-end

# View chunks from multiple files (multiple --id flags)
monodex view --id 30440fb2ecd5fa62:3 --id a1b2c3d4e5f67890:1-2

# Show full filesystem paths
monodex view --id 30440fb2ecd5fa62 --full-paths

# Omit catalog preamble (chunks only)
monodex view --id 30440fb2ecd5fa62 --chunks-only
```

### Debug chunking algorithm

```bash
# See how a file gets chunked (AST-only mode, reveals partitioner issues)
monodex dump-chunks --file ./src/JsonFile.ts

# Include fallback line-based splitting (production behavior)
monodex dump-chunks --file ./src/JsonFile.ts --with-fallback

# Visualize mode - show full chunk contents
monodex dump-chunks --file ./src/JsonFile.ts --visualize

# Debug mode - show partitioning decisions
monodex dump-chunks --file ./src/JsonFile.ts --debug

# Custom target chunk size (default: 6000 chars)
monodex dump-chunks --file ./src/JsonFile.ts --target-size 4000

# Audit chunking quality across multiple files (AST-only mode)
monodex audit-chunks --count 20 --dir /path/to/project
```

**Chunk Quality Score**: 0-100%, higher is better. Scores below 95% may indicate chunking issues. Note: `dump-chunks` and `audit-chunks` use AST-only mode (fallback disabled) to accurately measure partitioner quality.

### Purge data

```bash
# Purge all chunks from a specific catalog
monodex purge --catalog rushstack

# Purge entire collection (all catalogs)
monodex purge --all
```

## Architecture

```
monodex/
├── src/
│   ├── main.rs                    # CLI entry point
│   └── engine/                    # Reusable indexing engine
│       ├── mod.rs                 # Module exports
│       ├── config.rs              # File exclusion rules
│       ├── chunker.rs             # File chunking dispatcher
│       ├── partitioner.rs         # AST-based TypeScript chunking
│       ├── markdown_partitioner.rs # Markdown heading-based chunking
│       ├── embedder.rs            # Single-threaded embedding (legacy)
│       ├── parallel_embedder.rs   # Parallel embedding with multiple ONNX sessions
│       ├── package_lookup.rs      # Package name resolution (walk up to package.json)
│       ├── uploader.rs            # Qdrant HTTP client
│       └── util.rs                # Hash utilities for chunk IDs
├── Cargo.toml                     # Dependencies
├── DESIGN.md                      # Design documentation
└── README.md
```

### Chunking Strategy

**TypeScript/TSX files** are chunked using AST-aware partitioning:
- Splits at semantic boundaries (functions, classes, methods, enums)
- Includes preceding JSDoc/TSDoc comments with each symbol
- Handles oversized functions by splitting at statement blocks
- Full breadcrumb context: `package:file:Class.method`

**Quality indicators in breadcrumbs:**
- No marker: Successful AST split with good chunk geometry
- `:[degraded-ast-split]`: AST split with poor geometry (tiny chunks)
- `:[fallback-split]`: No AST split found, used line-based recovery (failure mode)

**Markdown files** are split by heading hierarchy.

**JSON files** are skipped (low value for semantic search).

**Exclusions:** Folders like `node_modules` and files like `*.test.ts` are automatically skipped. Exclusion rules are currently hardcoded in `config.rs` but will be configurable in a future release.

## Chunk Size Target

- **Target**: 6000 characters (text only)
- **Fits**: 8192-token embedding model limit (jina-embeddings-v2-base-code)
- **Breadcrumb**: Extra overhead for navigation context

## Prerequisites

- **Qdrant**: Vector database running on localhost:6333
- **Rust**: 1.91+ (for edition 2024)
- **Model**: jina-embeddings-v2-base-code (auto-downloaded from HuggingFace to `models/`)

## Qdrant Setup

Create the collection before first use:

```bash
curl -X PUT "http://localhost:6333/collections/rushstack" \
  -H "Content-Type: application/json" \
  -d '{
    "vectors": {
      "size": 768,
      "distance": "Cosine"
    }
  }'
```

Verify the collection exists:

```bash
curl http://localhost:6333/collections/rushstack | jq '.result.status'
```

The collection uses:
- **768 dimensions** (jina-embeddings-v2-base-code output size)
- **Cosine distance** (best for semantic similarity)

## Development

```bash
# Build
cargo build --release

# Test
cargo test

# Run with logging
RUST_LOG=debug ./target/release/monodex crawl --catalog rushstack
```

## License

MIT

## Related

- [Qdrant](https://qdrant.tech/) - Vector similarity search engine
- [ONNX Runtime](https://onnxruntime.ai/) - Cross-platform ML inference
- [Rush Stack](https://rushstack.io/) - Monorepo toolkit for JavaScript/TypeScript
