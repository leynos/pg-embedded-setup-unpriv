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
                    ("PGPASSFILE", Some(value)) => {
                        snapshot.pgpassfile = Some(OsString::from(value))
                    }
                    ("PGPASSFILE", None) => snapshot.pgpassfile = None,
                    ("TZDIR", Some(value)) => snapshot.tzdir = Some(OsString::from(value)),
                    ("TZDIR", None) => snapshot.tzdir = None,
                    ("TZ", Some(value)) => snapshot.timezone = Some(OsString::from(value)),
                    ("TZ", None) => snapshot.timezone = None,
                    _ => {}
                }
                snapshot
            })
    }
}

#[cfg(test)]
const _: fn(&TestBootstrapEnvironment) -> EnvSnapshot = EnvSnapshot::from_environment;
