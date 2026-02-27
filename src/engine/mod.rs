//! Reusable indexing engine for Qdrant semantic search
//! 
//! This module contains general-purpose indexing logic that works
//! for any Rush monorepo. It is designed to be reusable across projects.
//! 
//! Repository-specific configuration lives in `../config.rs`

pub mod config;
pub mod embedder;
pub mod chunker;
pub mod partitioner;
pub mod uploader;

pub use config::{should_skip_path, get_chunk_strategy, ChunkingStrategy};
pub use embedder::EmbeddingGenerator;
pub use chunker::{Chunk, chunk_file};
pub use partitioner::{PartitionConfig, PartitionedChunk, partition_typescript};
pub use uploader::{QdrantUploader, SearchResult};
