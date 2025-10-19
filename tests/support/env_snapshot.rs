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

    #[allow(dead_code)] // Used by bootstrap scenarios; some tests import without invoking it.
    pub fn from_environment(environment: &TestBootstrapEnvironment) -> Self {
        environment
            .to_env()
            .into_iter()
            .fold(Self::default(), |mut snapshot, (key, value)| {
                let value = OsString::from(value);
                match key.as_str() {
                    "PGPASSFILE" => snapshot.pgpassfile = Some(value),
                    "TZDIR" => snapshot.tzdir = Some(value),
                    "TZ" => snapshot.timezone = Some(value),
                    _ => {}
                }
                snapshot
            })
    }
}
