//! Behaviour and unit coverage for the public `rstest` fixture.
#![cfg(unix)]

use pg_embedded_setup_unpriv::TestCluster;
use sandbox::TestSandbox;

#[path = "../support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "../support/cluster_skip.rs"]
mod cluster_skip;
#[path = "../support/env.rs"]
mod env;
#[path = "../support/env_isolation.rs"]
mod env_isolation;
#[path = "../support/sandbox.rs"]
mod sandbox;
#[path = "../support/serial.rs"]
mod serial;
#[path = "../support/skip.rs"]
mod skip;

mod process_utils;
mod steps;
mod unit_tests;
mod world;
