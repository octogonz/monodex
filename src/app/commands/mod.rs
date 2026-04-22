//! CLI command handlers.
//!
//! Edit here when: Adding a new command or modifying command dispatch.

pub mod audit_chunks;
pub mod crawl;
pub mod dump_chunks;
pub mod purge;
pub mod search;
pub mod use_cmd;
pub mod view;

// Re-export command entry points
// TODO: Uncomment as functions are moved in step 1.7
// pub use audit_chunks::run_audit_chunks;
// pub use crawl::{run_crawl_label, run_crawl_working_dir};
// pub use dump_chunks::run_dump_chunks;
// pub use purge::run_purge;
// pub use search::run_search;
// pub use use_cmd::run_use;
// pub use view::run_view;
