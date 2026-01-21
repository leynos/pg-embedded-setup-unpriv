//! Cache lookup, population, and validation operations.
//!
//! Provides functions for checking cache status, copying binaries from cache,
//! and populating the cache after downloads.

mod copy;
mod lookup;
mod populate;

pub use copy::copy_from_cache;
pub use lookup::{CacheLookupResult, check_cache, find_matching_cached_version, try_use_cache};
pub use populate::{populate_cache, try_populate_cache};

#[cfg(test)]
mod tests;
