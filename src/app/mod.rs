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
pub use context::{
    DefaultContext, DEFAULT_CONTEXT_PATH, load_default_context, save_default_context,
    resolve_label_context,
};
pub use util::chrono_timestamp;
