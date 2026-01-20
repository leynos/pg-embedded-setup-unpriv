//! Shared binary cache for `PostgreSQL` downloads.
//!
//! This module provides caching infrastructure to avoid repeated downloads of
//! `PostgreSQL` binaries when multiple tests or processes need the same version.
//! Binaries are stored in a user-specific cache directory and shared across test
//! runs.
//!
//! # Cache Location
//!
//! The cache directory is resolved in the following order:
//!
//! 1. `PG_BINARY_CACHE_DIR` environment variable if set
//! 2. `$XDG_CACHE_HOME/pg-embedded/binaries` if `XDG_CACHE_HOME` is set
//! 3. `~/.cache/pg-embedded/binaries` as fallback
//!
//! # Cross-Process Coordination
//!
//! The cache uses file-based locking to coordinate downloads across parallel
//! test runners. Locks are per-version, allowing different versions to be
//! downloaded concurrently.

mod config;
mod lock;
mod operations;

pub use config::{BinaryCacheConfig, resolve_cache_dir};
pub use lock::CacheLock;
pub use operations::{
    CacheLookupResult, check_cache, copy_from_cache, find_matching_cached_version, populate_cache,
    try_populate_cache, try_use_cache,
};
