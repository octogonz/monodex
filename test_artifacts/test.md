# API Documentation

This document describes the core APIs for the monodex project.

## Installation

To install monodex, run the following command:

```bash
cargo install monodex
```

Make sure you have Rust 1.70 or later installed.

## Configuration

Create a configuration file at `~/.config/monodex/config.json`:

```json
{
  "qdrant": {
    "url": "http://localhost:6333",
    "collection": "rush-stack"
  },
  "catalogs": {
    "node-core-library": {
      "type": "filesystem",
      "path": "/path/to/rushstack/libraries/node-core-library"
    }
  }
}
```

### Environment Variables

You can also use environment variables:

- `QDRANT_URL` - Qdrant server URL
- `QDRANT_COLLECTION` - Collection name

## Usage

### Indexing

To index a catalog:

```bash
monodex crawl --catalog node-core-library
```

This will:
1. Scan all TypeScript files
2. Generate embeddings
3. Upload to Qdrant

### Querying

To search the index:

```bash
monodex query --text "how to read a JSON file"
```

Results are returned with:
- Breadcrumb path
- Code preview
- Line numbers

## Advanced Topics

### Custom Chunking

The partition algorithm splits code at AST boundaries:

1. **Functions** - Split at statement blocks
2. **Classes** - Split at method boundaries
3. **Enums** - Split at member boundaries

### Embedding Model

We use `BAAI/bge-small-en-v1.5` which provides:
- 384-dimensional vectors
- 512 token maximum
- Good semantic search quality

## Troubleshooting

### Common Issues

**Q: Embeddings are slow**

A: The first run downloads the model (~50MB). Subsequent runs are faster.

**Q: Out of memory**

A: Reduce batch size with `--batch-size 16`.

**Q: Connection refused**

A: Make sure Qdrant is running on the configured URL.
