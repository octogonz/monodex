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
    CatalogConfig, Config, EmbeddingModelConfig, EmbeddingSizeValue, QdrantConfig, load_config,
    print_memory_warning, resolve_embedding_config,
};
pub use context::{
    DEFAULT_CONTEXT_PATH, DefaultContext, load_default_context, resolve_label_context,
    save_default_context,
};
pub use util::chrono_timestamp;
