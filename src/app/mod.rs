//! Application-level concerns: CLI, config, commands, crawl pipeline.
//!
//! Edit here when: Adding or modifying CLI commands, user-facing config,
//! or the high-level crawl orchestration.

pub mod cli;
pub mod commands;
pub mod config;
pub mod context;
pub mod crawl;
pub mod util;

pub use cli::{Cli, Commands, CrawlSourceArgs};
pub use config::{
    Config, CatalogConfig, EmbeddingModelConfig, EmbeddingSizeValue, QdrantConfig,
    load_config, resolve_embedding_config, print_memory_warning,
};
