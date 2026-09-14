#![deny(clippy::all)]

//! assetvault — a small Node native addon that combines:
//!   * an in-memory multilingual full-text index (memory-indexer), and
//!   * a SQLite-backed content-addressed asset vault (assetpack-core).
//!
//! Design note: the binding deliberately stays small. Rich objects cross the
//! boundary as JSON strings, and binary payloads as Buffers. There is no custom
//! field abstraction beyond a minimal `{name, type, flags}` schema list.

mod index;
mod vault;

pub use index::SearchIndex;
pub use vault::SqliteVault;
