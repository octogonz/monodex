//! Crawl pipeline implementation.
//!
//! Edit here when: Modifying the embed/upload pipeline or crawl types.

pub mod pipeline;
pub mod types;

pub use pipeline::run_embed_upload_pipeline;
pub use types::{CrawlFailures, CrawlFileEntry, CrawlSource};
