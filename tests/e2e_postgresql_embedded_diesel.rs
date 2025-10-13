//! Exercises `postgresql-embedded` end-to-end while relying on
//! `pg-embedded-setup-unpriv` to bootstrap directories as root before
//! downgrading to the `nobody` user for database operations.
#![cfg(unix)]

use color_eyre::eyre::{Context, Result};
use diesel::prelude::*;
use diesel::sql_types::{Int4, Text};
use nix::unistd::geteuid;
use ortho_config::OrthoConfig;
use pg_embedded_setup_unpriv::{PgEnvCfg, nobody_uid, with_temp_euid};
use postgresql_embedded::PostgreSQL;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::runtime::Builder;

#[derive(QueryableByName, Debug, PartialEq, Eq)]
struct GreetingRow {
    #[diesel(sql_type = Int4)]
    id: i32,
    #[diesel(sql_type = Text)]
    message: String,
}

#[test]
fn e2e_postgresql_embedded_creates_and_queries_via_diesel() -> Result<()> {
    if !geteuid().is_root() {
        eprintln!("Skipping root-dependent PostgreSQL e2e test.");
        return Ok(());
    }

    const VERSION_REQ: &str = "=16.4.0";
    const PORT: u16 = 55_432;
    const PASSWORD: &str = "diesel_pass";
    const DATABASE_NAME: &str = "diesel_demo";
    const TABLE_SQL: &str = "CREATE TABLE greetings (id SERIAL PRIMARY KEY, message TEXT NOT NULL)";
    const INSERT_SQL: &str = "INSERT INTO greetings (message) VALUES ($1)";
    const SELECT_SQL: &str = "SELECT id, message FROM greetings ORDER BY id";
    const MESSAGE: &str = "hello from diesel";

    let install_dir = PathBuf::from("/var/tmp/pg-embedded-setup-it/install");
    let data_dir = PathBuf::from("/var/tmp/pg-embedded-setup-it/data");

    remove_dir_if_present(&install_dir)?;
    remove_dir_if_present(&data_dir)?;

    set_env("PG_RUNTIME_DIR", install_dir.as_os_str());
    set_env("PG_DATA_DIR", data_dir.as_os_str());
    set_env("PG_VERSION_REQ", VERSION_REQ);
    set_env("PG_PORT", PORT.to_string());
    set_env("PG_SUPERUSER", "postgres");
    set_env("PG_PASSWORD", PASSWORD);

    if let Err(err) = pg_embedded_setup_unpriv::run() {
        let message = err.to_string();
        if message.contains("rate limit exceeded") {
            eprintln!("Skipping e2e postgres test: {message}");
            return Ok(());
        }
        return Err(err).wrap_err("initialise postgres environment");
    }

    let cfg = PgEnvCfg::load().wrap_err("reload pg settings from environment")?;
    let mut settings = cfg
        .to_settings()
        .wrap_err("convert environment to settings")?;
    settings.timeout = Some(Duration::from_secs(60));
    settings.password_file = install_dir.join(".pgpass");
    settings.password = PASSWORD.to_string();

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");
    let password_file = settings.password_file.clone();

    with_temp_euid(nobody_uid(), move || {
        fs::create_dir_all(&cache_dir).wrap_err("create cache directory for nobody")?;
        fs::create_dir_all(&runtime_dir).wrap_err("create runtime directory for nobody")?;
        let mut pgpass = fs::File::create(&password_file)
            .wrap_err("provision password file for embedded postgres")?;
        write!(pgpass, "{}", settings.password).wrap_err("write postgres password")?;
        pgpass.sync_all().wrap_err("flush password file")?;
        fs::set_permissions(&password_file, fs::Permissions::from_mode(0o600))
            .wrap_err("set permissions on password file")?;

        set_env("HOME", install_dir.as_os_str());
        set_env("XDG_CACHE_HOME", cache_dir.as_os_str());
        set_env("XDG_RUNTIME_DIR", runtime_dir.as_os_str());
        set_env("PGPASSFILE", password_file.as_os_str());
        set_env("TZDIR", "/usr/share/zoneinfo");
        set_env("TZ", "UTC");

        eprintln!(
            "postgresql install dir {}",
            settings.installation_dir.display()
        );
        eprintln!("postgresql data dir {}", settings.data_dir.display());

        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .wrap_err("initialise tokio runtime for postgresql-embedded e2e test")?;

        let (postgresql, database_url) = runtime.block_on(async {
            let mut postgresql = PostgreSQL::new(settings.clone());
            postgresql.setup().await?;
            postgresql.start().await?;
            postgresql.create_database(DATABASE_NAME).await?;
            let database_url = postgresql.settings().url(DATABASE_NAME);
            Ok::<_, color_eyre::Report>((postgresql, database_url))
        })?;

        let mut connection = PgConnection::establish(&database_url)
            .wrap_err("connect to embedded postgres via diesel")?;

        diesel::sql_query(TABLE_SQL)
            .execute(&mut connection)
            .wrap_err("create greetings table")?;

        diesel::sql_query(INSERT_SQL)
            .bind::<Text, _>(MESSAGE)
            .execute(&mut connection)
            .wrap_err("insert greeting row")?;

        let rows: Vec<GreetingRow> = diesel::sql_query(SELECT_SQL)
            .load(&mut connection)
            .wrap_err("select greetings via diesel")?;

        assert_eq!(
            rows,
            vec![GreetingRow {
                id: 1,
                message: MESSAGE.to_string()
            }]
        );

        runtime
            .block_on(postgresql.stop())
            .wrap_err("stop embedded postgres instance")?;

        Ok(())
    })
}

fn remove_dir_if_present(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

fn set_env<K, V>(key: K, value: V)
where
    K: AsRef<str>,
    V: Into<OsString>,
{
    // Environment variables drive both pg-embedded-setup-unpriv and the later settings reload.
    unsafe { env::set_var(key.as_ref(), value.into()) }
}
