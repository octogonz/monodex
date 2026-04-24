//! LanceDB storage layer for monodex.
//!
//! Purpose: Provide a clean typed API for reading/writing chunks and label metadata.
//! This is the narrow seam between the application and LanceDB.
//!
//! Edit here when: Adding new storage operations, changing LanceDB table schemas,
//!   or modifying the database open/validate logic.
//! Do not edit here for: Chunking logic (see engine/partitioner/), CLI handlers (see app/commands/).

mod database;
mod rows;

#[cfg(test)]
mod api_smoke;

pub use database::{Database, META_FILE, MetaFile};
pub use rows::{ChunkRow, LabelMetadataRow, ScoredChunkRow};
