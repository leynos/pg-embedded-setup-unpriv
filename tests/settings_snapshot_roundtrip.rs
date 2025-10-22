//! Validates that worker settings snapshots faithfully round-trip PostgreSQL settings.
use std::collections::HashMap;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr};
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

    assert_eq!(restored.releases_url, expected.releases_url);
    assert_eq!(restored.version, expected.version);
    assert_eq!(restored.installation_dir, expected.installation_dir);
    assert_eq!(restored.password_file, expected.password_file);
    assert_eq!(restored.data_dir, expected.data_dir);
    assert_eq!(restored.host, expected.host);
    assert_eq!(restored.port, expected.port);
    assert_eq!(restored.username, expected.username);
    assert_eq!(restored.password, expected.password);
    assert_eq!(restored.temporary, expected.temporary);
    assert_eq!(restored.timeout, expected.timeout);
    assert_eq!(restored.configuration, expected.configuration);
    assert_eq!(
        restored.trust_installation_dir,
        expected.trust_installation_dir
    );

    Ok(())
}
