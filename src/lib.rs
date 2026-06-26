//! sedum — a filesystem-owned personal Markdown wiki.
//!
//! Markdown files under `sedum/` are the source of truth; the Postgres index
//! is a disposable cache rebuildable from `sedum/**/*.md`.

pub use anyhow::{bail, Context, Result};

pub mod indexer;
pub mod markdown;
