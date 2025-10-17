//! Capability helpers for the settings integration suite.
//!
//! The settings tests only need capability metadata and temporary directory guards to
//! validate configuration behaviour. This module re-exports the shared helpers from
//! `pg_embedded_setup_unpriv::test_support` to keep the suite's namespace focused on
//! settings-specific assertions.

pub use pg_embedded_setup_unpriv::test_support::{CapabilityTempDir, metadata};
