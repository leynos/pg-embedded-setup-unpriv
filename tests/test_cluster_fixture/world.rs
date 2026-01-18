//! Test cluster fixture utilities for managing test worlds.

use std::{any::Any, cell::RefCell, ffi::OsString, fs};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Result, eyre};
use rstest::fixture;

use super::{
    TestCluster, cap_fs,
    cluster_skip::cluster_skip_message,
    env::ScopedEnvVars,
    env_isolation::{override_env_os, override_env_path},
    process_utils::prepare_read_only_dir,
    sandbox::TestSandbox,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub(super) enum FixtureEnvProfile {
    #[default]
    Default,
    MissingTimezone,
    MissingWorkerBinary,
    NonExecutableWorkerBinary,
    PermissionDenied,
    ReadOnlyFilesystem,
    InvalidConfiguration,
}

pub(super) struct FixtureWorld {
    pub(super) sandbox: TestSandbox,
    cluster: Option<TestCluster>,
    panic_message: Option<String>,
    skip_reason: Option<String>,
    pub(super) env_profile: FixtureEnvProfile,
}

impl FixtureWorld {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("rstest-fixture").context("create fixture sandbox")?,
            cluster: None,
            panic_message: None,
            skip_reason: None,
            env_profile: FixtureEnvProfile::Default,
        })
    }

    pub(super) fn mark_skip(&mut self, reason: impl Into<String>) {
        let message = reason.into();
        tracing::warn!("{message}");
        self.skip_reason = Some(message);
    }

    pub(super) const fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    pub(super) fn ensure_not_skipped(&self) -> Result<()> {
        if self.is_skipped() {
            Err(eyre!("scenario skipped"))
        } else {
            Ok(())
        }
    }

    pub(super) fn record_cluster(&mut self, cluster: TestCluster) {
        self.cluster = Some(cluster);
        self.panic_message = None;
        self.skip_reason = None;
    }

    pub(super) fn record_failure(&mut self, payload: Box<dyn Any + Send>) {
        let message = panic_payload_to_string(payload);
        self.cluster = None;
        if let Some(reason) = cluster_skip_message(&message, None) {
            self.mark_skip(reason);
        } else {
            self.panic_message = Some(message);
        }
    }

    pub(super) fn cluster(&self) -> Result<&TestCluster> {
        self.ensure_not_skipped()?;
        self.cluster
            .as_ref()
            .ok_or_else(|| eyre!("test_cluster fixture did not yield a cluster"))
    }

    pub(super) fn panic_message(&self) -> Result<&str> {
        self.ensure_not_skipped()?;
        self.panic_message
            .as_deref()
            .ok_or_else(|| eyre!("fixture should have recorded a panic"))
    }
}

impl Drop for FixtureWorld {
    fn drop(&mut self) {
        drop(self.cluster.take());
    }
}

pub(super) type FixtureWorldFixture = Result<RefCell<FixtureWorld>>;

pub(super) fn borrow_world(world: &FixtureWorldFixture) -> Result<&RefCell<FixtureWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("fixture world failed to initialise: {err}")))
}

#[fixture]
pub(super) fn world() -> FixtureWorldFixture {
    Ok(RefCell::new(FixtureWorld::new()?))
}

pub(super) fn env_for_profile(
    sandbox: &TestSandbox,
    profile: FixtureEnvProfile,
) -> Result<ScopedEnvVars> {
    match profile {
        FixtureEnvProfile::Default => Ok(sandbox.env_without_timezone()),
        FixtureEnvProfile::MissingTimezone => {
            let missing_dir = sandbox.install_dir().join("missing-tz");
            Ok(sandbox.env_with_timezone_override(missing_dir.as_ref()))
        }
        FixtureEnvProfile::MissingWorkerBinary => {
            let mut vars = sandbox.env_without_timezone();
            let fake_worker = sandbox
                .install_dir()
                .join("missing-worker")
                .join("pg_worker");
            override_env_os(
                &mut vars,
                "PG_EMBEDDED_WORKER",
                Some(OsString::from(fake_worker.as_str())),
            );
            Ok(vars)
        }
        FixtureEnvProfile::NonExecutableWorkerBinary => {
            let mut vars = sandbox.env_without_timezone();
            let worker_dir = sandbox.install_dir().join("non-exec-worker");
            let worker_path = worker_dir.join("pg_worker");
            std::fs::create_dir_all(worker_dir.as_std_path())
                .with_context(|| format!("create non-exec worker dir at {worker_dir}"))?;
            fs::write(worker_path.as_std_path(), "#!/bin/sh\nexit 0\n")
                .with_context(|| format!("write stub worker at {worker_path}"))?;
            cap_fs::set_permissions(worker_path.as_ref(), 0o644)
                .with_context(|| format!("strip execution bit from {worker_path}"))?;
            override_env_os(
                &mut vars,
                "PG_EMBEDDED_WORKER",
                Some(OsString::from(worker_path.as_str())),
            );
            Ok(vars)
        }
        FixtureEnvProfile::PermissionDenied => {
            let mut vars = sandbox.env_without_timezone();
            let runtime = sandbox.install_dir().join("denied-runtime");
            let data = sandbox.install_dir().join("denied-data");
            create_dir_with_mode(&runtime, 0o000)?;
            create_dir_with_mode(&data, 0o000)?;
            override_env_path(&mut vars, "PG_RUNTIME_DIR", runtime.as_ref());
            override_env_path(&mut vars, "PG_DATA_DIR", data.as_ref());
            Ok(vars)
        }
        FixtureEnvProfile::ReadOnlyFilesystem => {
            let mut vars = sandbox.env_without_timezone();
            let runtime = sandbox.install_dir().join("readonly-runtime");
            let data = sandbox.install_dir().join("readonly-data");
            prepare_read_only_dir(&runtime)?;
            prepare_read_only_dir(&data)?;
            override_env_path(&mut vars, "PG_RUNTIME_DIR", runtime.as_ref());
            override_env_path(&mut vars, "PG_DATA_DIR", data.as_ref());
            Ok(vars)
        }
        FixtureEnvProfile::InvalidConfiguration => {
            let mut vars = sandbox.env_without_timezone();
            override_env_os(&mut vars, "PG_PORT", Some(OsString::from("not-a-port")));
            Ok(vars)
        }
    }
}

fn create_dir_with_mode(path: &Utf8PathBuf, mode: u32) -> Result<()> {
    std::fs::create_dir_all(path.as_std_path())
        .with_context(|| format!("create permission sandbox dir at {path}"))?;
    cap_fs::set_permissions(path.as_ref(), mode)
        .with_context(|| format!("set permissions {mode:o} for {path}"))
}

pub(super) fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(fallback) => fallback.downcast::<&'static str>().map_or_else(
            |_| "non-string panic payload".to_owned(),
            |message| (*message).to_owned(),
        ),
    }
}

pub(super) fn handle_fixture_panic(payload: Box<dyn Any + Send>) -> Result<()> {
    let message = panic_payload_to_string(payload);
    cluster_skip_message(&message, None).map_or_else(
        || Err(eyre!(message)),
        |reason| {
            tracing::warn!("{reason}");
            Ok(())
        },
    )
}
