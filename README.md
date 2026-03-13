# rush-qdrant

**Semantic search indexer for Rush monorepos using Qdrant vector database**

## Overview

`rush-qdrant` is a production-quality CLI tool that indexes Rush monorepo source code and documentation into a Qdrant vector database for high-quality semantic search.

### Features

- **AST-based chunking**: Tree-sitter powered intelligent splitting for TypeScript/TSX files
- **Breadcrumb context**: Full symbol paths like `@rushstack/node-core-library:JsonFile.ts:JsonFile.load`
- **Oversized chunk handling**: Functions split at natural AST boundaries (statement blocks, if/else, try/catch)
- **Local embeddings**: Uses jina-embeddings-v2-base-code with ONNX Runtime (no external APIs)
- **Qdrant integration**: Direct batch uploads to Qdrant vector database
- **Incremental sync**: Content-hash based change detection for fast re-indexing
- **Rush-optimized**: Smart exclusion rules for Rush monorepo patterns

## Installation

```bash
# Build from source
cargo build --release

# Binary will be at ./target/release/rush-qdrant
```

## Usage

### Configuration

Create `~/.config/rush-qdrant/config.jsonc`:

```json
{
  "qdrant": {
    "url": "http://localhost:6333",
    "collection": "rushstack"
  },
  "catalogs": {
    "rushstack": {
      "type": "monorepo",
      "path": "/path/to/rushstack",
      "package_name": "@rushstack"
    }
  }
}
```

### Index a repository

```bash
# Index using config file
rush-qdrant crawl --catalog rushstack

# With custom config path
rush-qdrant --config /path/to/config.jsonc crawl --catalog rushstack
```

### Search the database

```bash
# Semantic search with compact blurb output (for AI assistants)
rush-qdrant search --text "how to read JSON files"

# With catalog filter and limit
rush-qdrant search --text "API Extractor" --catalog rushstack --limit 10
```

### View full chunks

```bash
# View a single chunk by ID
rush-qdrant view --id 30440fb2ecd5fa62

# View multiple chunks (comma-separated)
rush-qdrant view --id 30440fb2ecd5fa62,a1b2c3d4e5f67890
```

### Debug chunking algorithm

```bash
# See how a file gets chunked (AST-only mode, reveals partitioner issues)
rush-qdrant dump-chunks --file ./src/JsonFile.ts

# Include fallback line-based splitting (production behavior)
rush-qdrant dump-chunks --file ./src/JsonFile.ts --with-fallback

# Visualize mode - show full chunk contents
rush-qdrant dump-chunks --file ./src/JsonFile.ts --visualize

# Audit chunking quality across multiple files (AST-only mode)
rush-qdrant audit-chunks --count 20

# Audit from a specific directory
rush-qdrant audit-chunks --count 50 --dir /path/to/project
```

**Chunk Quality Score**: 0-100%, higher is better. Scores below 95% may indicate chunking issues. Note: `dump-chunks` and `audit-chunks` use AST-only mode (fallback disabled) to accurately measure partitioner quality.

### Verbose query (for debugging)

```bash
# Verbose output for debugging search behavior
rush-qdrant query --text "how to read JSON files"
```

## Architecture

```
rush-qdrant/
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
RUST_LOG=debug ./target/release/rush-qdrant crawl --catalog rushstack
```

## License

MIT

## Related

- [Qdrant](https://qdrant.tech/) - Vector similarity search engine
- [ONNX Runtime](https://onnxruntime.ai/) - Cross-platform ML inference
- [Rush Stack](https://rushstack.io/) - Monorepo toolkit for JavaScript/TypeScript
