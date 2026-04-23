//! Crawl-related types shared across command handlers.
//!
//! Edit here when: Adding new crawl source types or failure tracking.
//! Do not edit here for: Pipeline logic (see `pipeline.rs`).

/// Source type for crawling
/// (Prepared for future refactoring to further unify crawl entry points)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum CrawlSource {
    /// Git commit-based crawling
    GitCommit { commit_oid: String },
    /// Working directory crawling (uncommitted changes)
    WorkingDirectory,
}

impl CrawlSource {
    #[allow(dead_code)]
    /// Get the source kind string for label metadata
    pub fn source_kind(&self) -> &'static str {
        match self {
            CrawlSource::GitCommit { .. } => "git-commit",
            CrawlSource::WorkingDirectory => "working-directory",
        }
    }

    /// Get the commit OID (empty string for working directory)
    #[allow(dead_code)]
    pub fn commit_oid(&self) -> &str {
        match self {
            CrawlSource::GitCommit { commit_oid } => commit_oid,
            CrawlSource::WorkingDirectory => "",
        }
    }
}

/// File entry from crawl source
/// (Prepared for future refactoring to further unify crawl entry points)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CrawlFileEntry {
    pub relative_path: String,
    pub blob_id: String,
}

/// Failure tracking for crawl pipeline
#[derive(Debug, Default)]
pub struct CrawlFailures {
    pub upload_failures: Vec<String>,
    pub file_complete_failures: Vec<String>,
    pub label_add_failures: Vec<String>,
    pub embedding_failures: Vec<String>,
}

impl CrawlFailures {
    pub fn total(&self) -> usize {
        self.upload_failures.len()
            + self.file_complete_failures.len()
            + self.label_add_failures.len()
            + self.embedding_failures.len()
    }

    pub fn has_failures(&self) -> bool {
        self.total() > 0
    }
}
