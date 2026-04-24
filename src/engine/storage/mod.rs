//! LanceDB storage layer for monodex.
//!
//! Edit here when: Adding new storage operations, changing LanceDB table schemas,
//!   or modifying the database open/validate logic.
//! Do not edit here for: Chunking logic (see engine/partitioner/), CLI handlers (see app/commands/).

#[cfg(test)]
mod api_smoke;
