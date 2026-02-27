# rush-qdrant

**Semantic search indexer for Rush monorepos using Qdrant vector database**

## Overview

`rush-qdrant` is a production-quality CLI tool that indexes Rush monorepo source code and documentation into a Qdrant vector database for high-quality semantic search.

### Features

- **Intelligent chunking**: Different strategies for TypeScript, JavaScript, Markdown, JSON, YAML
- **Local embeddings**: Uses BAAI/bge-small-en-v1.5 model with Candle ML (no external APIs)
- **Qdrant integration**: Direct batch uploads to Qdrant vector database
- **Rush-optimized**: Configurable exclusion rules for Rush monorepo patterns
- **Production-ready**: Single Rust binary, no Python runtime required

## Installation

```bash
# Build from source
cargo build --release

# Binary will be at ./target/release/rush-qdrant
```

## Usage

### Index a repository

```bash
# Index RushStack monorepo
rush-qdrant index --directory ./rushstack --collection rushstack-ai

# With custom chunk size
rush-qdrant index --directory ./rushstack --collection rushstack-ai --chunk-lines 100
```

### Query the database

```bash
# Semantic search
rush-qdrant query --text "how to read JSON files" --collection rushstack-ai

# With custom limit
rush-qdrant query --text "API Extractor" --collection rushstack-ai --limit 10
```

## Architecture

```
rush-qdrant/
├── src/
│   ├── main.rs                    # CLI entry point
│   └── engine/                    # Reusable indexing engine
│       ├── mod.rs                 # Module exports
│       ├── config.rs              # Repository-specific rules (EDIT THIS)
│       ├── chunker.rs             # File chunking logic
│       └── embedder.rs            # Embedding generation (Candle)
├── Cargo.toml                     # Dependencies
└── README.md
```

### Configuration

Edit `src/engine/config.rs` to customize:
- File exclusion rules (`should_skip_path`)
- Chunking strategies (`get_chunk_strategy`)
- Repository-specific patterns

Future: Will support `.rush-qdrant/config.jsonc` for easier configuration.

## Current Status

**Phase 1**: ✅ Complete - Qdrant setup and basic CLI structure

**Phase 2**: 🚧 In Progress - Intelligent chunking and indexing

**TODO**:
- [ ] Tree-sitter integration for TypeScript/JavaScript AST-based chunking
- [ ] Markdown heading-based splitting
- [ ] JSON 2-level key splitting
- [ ] Qdrant batch upload implementation
- [ ] Query command implementation
- [ ] Snapshot export/import
- [ ] Configuration file support (.jsonc)

## Prerequisites

- **Qdrant**: Vector database running on localhost:6333
- **Rust**: 1.91+ (for edition 2024)
- **Model**: BAAI/bge-small-en-v1.5 (auto-downloaded from HuggingFace)

## Development

```bash
# Build
cargo build --release

# Test
cargo test

# Run
./target/release/rush-qdrant --help
```

## License

MIT

## Related

- [Qdrant](https://qdrant.tech/) - Vector similarity search engine
- [Candle](https://github.com/huggingface/candle) - Minimalist ML framework for Rust
- [Rush Stack](https://rushstack.io/) - Monorepo toolkit for JavaScript/TypeScript
