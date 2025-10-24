//! Validates that worker settings snapshots faithfully round-trip `PostgreSQL` settings.
use std::collections::HashMap;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, ensure};
use pg_embedded_setup_unpriv::worker::SettingsSnapshot;
use postgresql_embedded::{Settings, VersionReq};

#[test]
fn settings_snapshot_roundtrip_preserves_all_fields() -> Result<()> {
    let mut configuration = HashMap::new();
    configuration.insert("log_min_messages".into(), "debug".into());
    configuration.insert("shared_buffers".into(), "128MB".into());

    let settings = Settings {
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
    };

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
