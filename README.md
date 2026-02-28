# rush-qdrant

**Semantic search indexer for Rush monorepos using Qdrant vector database**

## Overview

`rush-qdrant` is a production-quality CLI tool that indexes Rush monorepo source code and documentation into a Qdrant vector database for high-quality semantic search.

### Features

- **AST-based chunking**: Tree-sitter powered intelligent splitting for TypeScript/TSX files
- **Breadcrumb context**: Full symbol paths like `@rushstack/node-core-library:JsonFile.ts:JsonFile.load`
- **Oversized chunk handling**: Functions split at natural AST boundaries (statement blocks, if/else, try/catch)
- **Local embeddings**: Uses BAAI/bge-small-en-v1.5 model with Candle ML (no external APIs)
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

### Query the database

```bash
# Semantic search
rush-qdrant query --text "how to read JSON files"

# With catalog filter
rush-qdrant query --text "API Extractor" --catalog rushstack --limit 10
```

### Debug chunking algorithm

```bash
# See how a file gets chunked
rush-qdrant dump-chunks --file ./src/JsonFile.ts --package "@rushstack/node-core-library"
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
│       ├── embedder.rs            # Embedding generation (Candle)
│       └── uploader.rs            # Qdrant HTTP client
├── Cargo.toml                     # Dependencies
└── README.md
```

### Chunking Strategy

**TypeScript/TSX files** are chunked using AST-aware partitioning:
- Splits at semantic boundaries (functions, classes, methods, enums)
- Includes preceding JSDoc/TSDoc comments with each symbol
- Handles oversized functions by splitting at statement blocks
- Full breadcrumb context: `package:file:Class.method`

**Markdown files** are split by heading hierarchy.

**JSON files** are skipped (low value for semantic search).

## Chunk Size Target

- **Target**: 1800 characters (text only)
- **Fits**: 512-token embedding model limit (BAAI/bge-small-en-v1.5)
- **Breadcrumb**: Extra overhead for navigation context

## Prerequisites

- **Qdrant**: Vector database running on localhost:6333
- **Rust**: 1.91+ (for edition 2024)
- **Model**: BAAI/bge-small-en-v1.5 (auto-downloaded from HuggingFace to `models/`)

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
- [Candle](https://github.com/huggingface/candle) - Minimalist ML framework for Rust
- [Rush Stack](https://rushstack.io/) - Monorepo toolkit for JavaScript/TypeScript
