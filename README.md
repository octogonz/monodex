<div>
  <br />
  <a href="https://github.com/microsoft/monodex">
    <img height="130" alt="Rush Monodex" src="./assets/monodex-logo.svg">
  </a>
  <p />
</div>

# Rush Monodex

**Semantic search indexer for Rush monorepos using Qdrant vector database**

## Overview

`monodex` is a CLI tool that indexes Rush monorepo source code and documentation into a Qdrant vector database for scalable semantic search. It supports **label-based indexing**, allowing you to maintain multiple queryable snapshots (branches, commits) within a single catalog.

### Features

- **Label-based indexing**: Maintain multiple queryable filesets (branches, commits) within a catalog
- **Commit-based crawling**: Reads directly from Git objects, not working tree (deterministic, reproducible)
- **AST-based chunking**: Tree-sitter powered intelligent splitting for TypeScript/TSX files
- **Breadcrumb context**: Full symbol paths like `@rushstack/node-core-library:JsonFile.ts:JsonFile.load`
- **Oversized chunk handling**: Functions split at natural AST boundaries (statement blocks, if/else, try/catch)
- **Local embeddings**: Uses jina-embeddings-v2-base-code with ONNX Runtime (no external APIs)
- **Qdrant integration**: Direct batch uploads to Qdrant vector database
- **Incremental sync**: Content-hash based change detection for fast re-indexing
- **Intelligent deduplication**: Identical content at same path across labels shares chunks
- **Rush-optimized**: Smart exclusion rules for Rush monorepo patterns

## Agent Usage Guide

This tool is designed for AI assistants. The indexed database provides a complete, internally consistent snapshot of the codebase as it existed at crawl time — independent of any local file changes, branches, or whether the repo is even cloned. This makes it more than a replacement for grep; it can be the primary way an agent learns about a codebase.

**Typical workflow:**

1. **Set default context** (optional but recommended):
   ```bash
   monodex use --catalog rushstack --label main
   ```

2. **Start with semantic search** to find relevant code:
   ```bash
   monodex search --text "how does rush handle pnpm shrinkwrap files"
   ```

3. **View full chunks** using the `file_id:chunk_ordinal` from search results:
   ```bash
   monodex view --id 700a4ba232fe9ddc:3
   ```

4. **Get surrounding context** by viewing adjacent chunks:
   ```bash
   monodex view --id 700a4ba232fe9ddc:2-4
   ```

5. **Reconstruct entire files** by viewing all chunks:
   ```bash
   monodex view --id 700a4ba232fe9ddc
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

**Note:** Use `sparo` (or another small monorepo) for development testing. `rushstack` is used for final verification and takes hours to crawl.

**Fields:**

| Field                  | Required | Description                                          |
| ---------------------- | -------- | ---------------------------------------------------- |
| `qdrant.url`           | No       | Qdrant server URL (default: `http://localhost:6333`) |
| `qdrant.collection`    | Yes      | Qdrant collection name                               |
| `catalogs.<name>.type` | Yes      | Catalog type: `"monorepo"` or `"folder"`             |
| `catalogs.<name>.path` | Yes      | Absolute path to the repository root                 |

**Catalog types:**

- **`monorepo`**: Walks upward to find the nearest `package.json` for package name resolution. Breadcrumbs show `@scope/package-name:File.ts:Symbol`.
- **`folder`**: Uses the parent folder name as the package identifier. Breadcrumbs show `folder-name:File.ts:Symbol`.

### Label-Based Indexing

A **label** is a named, queryable fileset within a catalog. Labels typically represent branches or specific commits:

- `rushstack:main` - main branch
- `rushstack:feature-x` - feature branch
- `rushstack:v1.0.0` - specific release tag

**Key concept:** Chunks are immutable content. Labels track which chunks belong to which fileset. When you crawl a new commit under a label, membership is updated but identical content is reused.

### Set Default Context

```bash
# Set default catalog and label for subsequent commands
monodex use --catalog rushstack --label main

# Now you can omit --catalog and --label flags
monodex search --text "how to read JSON files"
```

Default context is stored in `~/.config/monodex/context.json`. Explicit flags always override defaults.

### Index a Repository

```bash
# Index HEAD commit under the "main" label
monodex crawl --catalog rushstack --label main

# Index a specific commit or branch
monodex crawl --catalog rushstack --label feature-x --commit feature-branch

# Index a specific commit SHA
monodex crawl --catalog rushstack --label v1.0.0 --commit a1b2c3d4e5f6
```

**Incremental sync:** The crawl is incremental — unchanged files are skipped. You can safely CTRL+C and resume later.

**Commit-based:** Crawling reads from Git objects, not the working tree. Uncommitted changes are ignored. This ensures deterministic, reproducible indexing.

**Label reassignment:** When you re-crawl a label with a new commit, chunks from the old commit that no longer exist are removed from that label's membership.

### Search the Database

```bash
# Semantic search (uses default context if set)
monodex search --text "how to read JSON files"

# With explicit catalog and label
monodex search --text "API Extractor" --catalog rushstack --label main --limit 10
```

### View Full Chunks

```bash
# View a specific chunk by ordinal
monodex view --id 30440fb2ecd5fa62:3

# View a range of chunks
monodex view --id 30440fb2ecd5fa62:2-4

# View from chunk 3 to the end
monodex view --id 30440fb2ecd5fa62:3-end

# View all chunks in a file (reconstruct entire file)
monodex view --id 30440fb2ecd5fa62

# View chunks from multiple files
monodex view --id 30440fb2ecd5fa62:3 --id a1b2c3d4e5f67890:1-2

# Show full filesystem paths
monodex view --id 30440fb2ecd5fa62 --full-paths

# Omit catalog preamble (chunks only)
monodex view --id 30440fb2ecd5fa62 --chunks-only
```

### Debug Chunking Algorithm

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

### Purge Data

```bash
# Purge all chunks from a specific label
monodex purge --catalog rushstack --label main

# Purge all chunks from a catalog (all labels)
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
│       ├── git_ops.rs             # Git tree enumeration and blob reading
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
curl -X PUT "http://localhost:6333/collections/monodex" \
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
curl http://localhost:6333/collections/monodex | jq '.result.status'
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

# Run with logging (use sparo for testing, not rushstack)
RUST_LOG=debug ./target/release/monodex crawl --catalog sparo --label main
```

## License

MIT

## Related

- [Qdrant](https://qdrant.ai/) - Vector similarity search engine
- [ONNX Runtime](https://onnxruntime.ai/) - Cross-platform ML inference
- [Rush Stack](https://rushstack.io/) - Monorepo toolkit for JavaScript/TypeScript
