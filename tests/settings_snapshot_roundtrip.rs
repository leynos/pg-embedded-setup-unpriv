//! Validates that worker settings snapshots faithfully round-trip `PostgreSQL` settings.
use std::collections::HashMap;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, ensure};
use pg_embedded_setup_unpriv::worker::{SettingsSnapshot, WorkerPayload};
use postgresql_embedded::{Settings, VersionReq};

fn sample_settings() -> Result<Settings> {
    let mut configuration = HashMap::new();
    configuration.insert("log_min_messages".into(), "debug".into());
    configuration.insert("shared_buffers".into(), "128MB".into());

    Ok(Settings {
        releases_url: "https://example.invalid/releases".into(),
        version: VersionReq::parse("=16.4.0").wrap_err("parse version requirement")?,
        installation_dir: "/var/lib/postgres/install".into(),
        password_file: "/var/lib/postgres/.pgpass".into(),
        data_dir: "/var/lib/postgres/data".into(),
        host: "127.0.0.1".into(),
        port: 54_321,
        username: "postgres".into(),
        password: "secret".into(),
        temporary: true,
        timeout: Some(Duration::from_secs(45)),
        configuration,
        trust_installation_dir: true,
    })
}

#[test]
fn settings_snapshot_roundtrip_preserves_all_fields() -> Result<()> {
    let settings = sample_settings()?;

    let expected = settings.clone();

    let snapshot = SettingsSnapshot::try_from(&settings)?;
    let restored = snapshot.into_settings()?;

    let comparisons = [
        (
            "releases_url",
            restored.releases_url == expected.releases_url,
        ),
        ("version", restored.version == expected.version),
        (
            "installation_dir",
            restored.installation_dir == expected.installation_dir,
        ),
        (
            "password_file",
            restored.password_file == expected.password_file,
        ),
        ("data_dir", restored.data_dir == expected.data_dir),
        ("host", restored.host == expected.host),
        ("port", restored.port == expected.port),
        ("username", restored.username == expected.username),
        ("password", restored.password == expected.password),
        ("temporary", restored.temporary == expected.temporary),
        ("timeout", restored.timeout == expected.timeout),
        (
            "configuration",
            restored.configuration == expected.configuration,
        ),
        (
            "trust_installation_dir",
            restored.trust_installation_dir == expected.trust_installation_dir,
        ),
    ];

    for (field, matches) in comparisons {
        ensure!(matches, "{field} did not round-trip correctly");
    }

    Ok(())
}

#[test]
fn worker_payload_debug_redacts_sensitive_values() -> Result<()> {
    let settings = sample_settings()?;
    let environment = vec![
        ("PGPASSWORD".to_owned(), Some("supersecret".to_owned())),
        ("PATH".to_owned(), Some("/tmp/bin".to_owned())),
        ("EMPTY".to_owned(), None),
    ];

    let payload = WorkerPayload::new(&settings, environment)?;
    let debug_output = format!("{payload:?}");

    ensure!(
        debug_output.contains("PGPASSWORD=<redacted>"),
        "expected PGPASSWORD to be redacted"
    );
    ensure!(
        debug_output.contains("PATH=<redacted>"),
        "expected PATH to be redacted"
    );
    ensure!(
        debug_output.contains("EMPTY=<unset>"),
        "expected EMPTY to be marked as unset"
    );
    ensure!(
        !debug_output.contains("supersecret"),
        "debug output leaked the worker password"
    );
    ensure!(
        !debug_output.contains("/tmp/bin"),
        "debug output leaked environment contents"
    );
    ensure!(
        !debug_output.contains(&settings.password),
        "debug output leaked settings password"
    );

    Ok(())
}
