//! Captures environment state for behavioural assertions.

use std::ffi::OsString;

use pg_embedded_setup_unpriv::TestBootstrapEnvironment;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvSnapshot {
    pub pgpassfile: Option<OsString>,
    pub tzdir: Option<OsString>,
    pub timezone: Option<OsString>,
}

impl EnvSnapshot {
    pub fn capture() -> Self {
        Self {
            pgpassfile: std::env::var_os("PGPASSFILE"),
            tzdir: std::env::var_os("TZDIR"),
            timezone: std::env::var_os("TZ"),
        }
    }

    pub fn from_environment(environment: &TestBootstrapEnvironment) -> Self {
        environment
            .to_env()
            .into_iter()
            .fold(Self::default(), |mut snapshot, (key, value)| {
                match (key.as_str(), value) {
                    ("PGPASSFILE", Some(env_value)) => {
                        snapshot.pgpassfile = Some(OsString::from(env_value));
                    }
                    ("PGPASSFILE", None) => snapshot.pgpassfile = None,
                    ("TZDIR", Some(env_value)) => {
                        snapshot.tzdir = Some(OsString::from(env_value));
                    }
                    ("TZDIR", None) => snapshot.tzdir = None,
                    ("TZ", Some(env_value)) => {
                        snapshot.timezone = Some(OsString::from(env_value));
                    }
                    ("TZ", None) => snapshot.timezone = None,
                    _ => {}
                }
                snapshot
            })
    }
}

#[cfg(test)]
const _: fn(&TestBootstrapEnvironment) -> EnvSnapshot = EnvSnapshot::from_environment;
