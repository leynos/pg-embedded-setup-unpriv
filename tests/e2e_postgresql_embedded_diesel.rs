//! Exercises `postgresql-embedded` end-to-end while relying on
//! `pg-embedded-setup-unpriv` to bootstrap directories as root before
//! downgrading to the `nobody` user for database operations.
#![cfg(all(unix, feature = "privileged-tests"))]

use std::ffi::OsString;
use std::io::Write;
use std::time::Duration;

use camino::Utf8PathBuf;
use cap_std::fs::{OpenOptions, PermissionsExt};
use color_eyre::eyre::{Context, Result, eyre};
use diesel::prelude::*;
use diesel::sql_types::{Int4, Text};
use nix::unistd::geteuid;
use ortho_config::OrthoConfig;
use pg_embedded_setup_unpriv::{
    PgEnvCfg, nobody_uid, test_support::bootstrap_error, with_temp_euid,
};
use postgresql_embedded::PostgreSQL;
use tokio::runtime::Builder;

#[path = "support/mod.rs"]
mod support;

use support::{
    cap_fs::{ensure_dir, open_dir, remove_tree},
    env::with_scoped_env,
};

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

    let base = Utf8PathBuf::from(format!(
        "/var/tmp/pg-embedded-setup-it/e2e-{}",
        std::process::id()
    ));
    let install_dir = base.join("install");
    let data_dir = base.join("data");

    remove_tree(&base)?;

    with_scoped_env(
        [
            (
                OsString::from("PG_RUNTIME_DIR"),
                Some(OsString::from(install_dir.as_str())),
            ),
            (
                OsString::from("PG_DATA_DIR"),
                Some(OsString::from(data_dir.as_str())),
            ),
            (
                OsString::from("PG_VERSION_REQ"),
                Some(OsString::from(VERSION_REQ)),
            ),
            (
                OsString::from("PG_PORT"),
                Some(OsString::from(PORT.to_string())),
            ),
            (
                OsString::from("PG_SUPERUSER"),
                Some(OsString::from("postgres")),
            ),
            (
                OsString::from("PG_PASSWORD"),
                Some(OsString::from(PASSWORD)),
            ),
        ],
        || {
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
            let password_file = install_dir.join(".pgpass");
            settings.password_file = password_file.clone().into_std_path_buf();
            settings.password = PASSWORD.to_string();

            let cache_dir = install_dir.join("cache");
            let runtime_dir = install_dir.join("run");

            let cache_dir_for_nobody = cache_dir.clone();
            let runtime_dir_for_nobody = runtime_dir.clone();
            let password_file_for_nobody = password_file.clone();
            let install_dir_for_nobody = install_dir.clone();
            let settings_for_nobody = settings.clone();

            with_temp_euid(nobody_uid(), move || {
                (|| -> Result<()> {
                    ensure_dir(&cache_dir_for_nobody, 0o755)?;
                    ensure_dir(&runtime_dir_for_nobody, 0o755)?;

                    let install_handle = open_dir(&install_dir_for_nobody)
                        .context("open install directory for nobody")?;
                    let password_relative = password_file_for_nobody
                        .strip_prefix(&install_dir_for_nobody)
                        .map_err(|_| eyre!("password file must live inside install dir"))?;
                    let mut pgpass = install_handle
                        .open_with(
                            password_relative.as_std_path(),
                            OpenOptions::new().create(true).truncate(true).write(true),
                        )
                        .context("provision password file for embedded postgres")?;
                    write!(pgpass, "{}", settings_for_nobody.password)
                        .context("write postgres password")?;
                    pgpass.sync_all().context("flush password file")?;
                    install_handle
                        .set_permissions(
                            password_relative.as_std_path(),
                            cap_std::fs::Permissions::from_mode(0o600),
                        )
                        .context("set permissions on password file")?;

                    with_scoped_env(
                        [
                            (
                                OsString::from("HOME"),
                                Some(OsString::from(install_dir_for_nobody.as_str())),
                            ),
                            (
                                OsString::from("XDG_CACHE_HOME"),
                                Some(OsString::from(cache_dir_for_nobody.as_str())),
                            ),
                            (
                                OsString::from("XDG_RUNTIME_DIR"),
                                Some(OsString::from(runtime_dir_for_nobody.as_str())),
                            ),
                            (
                                OsString::from("PGPASSFILE"),
                                Some(OsString::from(password_file_for_nobody.as_str())),
                            ),
                            (
                                OsString::from("TZDIR"),
                                Some(OsString::from("/usr/share/zoneinfo")),
                            ),
                            (OsString::from("TZ"), Some(OsString::from("UTC"))),
                        ],
                        move || -> Result<()> {
                            eprintln!(
                                "postgresql install dir {}",
                                settings_for_nobody.installation_dir.display()
                            );
                            eprintln!(
                                "postgresql data dir {}",
                                settings_for_nobody.data_dir.display()
                            );

                            let runtime = Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .wrap_err(
                                    "initialise tokio runtime for postgresql-embedded e2e test",
                                )?;

                            let (postgresql, database_url) = runtime.block_on(async {
                                let mut postgresql = PostgreSQL::new(settings_for_nobody.clone());
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

                            Ok::<(), color_eyre::Report>(())
                        },
                    )?;

                    Ok::<(), color_eyre::Report>(())
                })()
                .map_err(bootstrap_error)
            })?;

            Ok(())
        },
    )
}
