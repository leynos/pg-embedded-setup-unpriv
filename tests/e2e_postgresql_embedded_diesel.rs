//! Exercises `postgresql-embedded` end-to-end while relying on
//! `pg-embedded-setup-unpriv` to bootstrap directories as root before
//! downgrading to the `nobody` user for database operations.
#![cfg(all(unix, feature = "privileged-tests"))]

use std::io::Write;
use std::time::Duration;

use camino::Utf8PathBuf;
use cap_std::fs::{OpenOptions, PermissionsExt};
use color_eyre::eyre::{Context, Result, ensure, eyre};
use diesel::prelude::*;
use diesel::sql_types::{Int4, Text};
use nix::unistd::{User, fchown, geteuid};
use pg_embedded_setup_unpriv::PgEnvCfg;
use pg_embedded_setup_unpriv::worker_process_test_api::WorkerOperation;
use postgresql_embedded::PostgreSQL;
use tokio::runtime::{Builder, Runtime};

#[path = "support/cap_fs_privileged.rs"]
mod cap_fs;
#[path = "support/diesel_e2e_helpers.rs"]
mod diesel_e2e_helpers;
#[path = "support/env.rs"]
mod env;

use cap_fs::{ensure_dir, open_dir, remove_tree};
use diesel_e2e_helpers::{
    PostgresHandle, WorkerHandle, ensure_database_exists, run_worker_operation, worker_from_env,
};
use env::{ScopedEnvVars, build_env, with_scoped_env};

#[derive(QueryableByName, Debug, PartialEq, Eq)]
struct GreetingRow {
    #[diesel(sql_type = Int4)]
    id: i32,
    #[diesel(sql_type = Text)]
    message: String,
}

#[derive(Debug)]
struct BootstrapContext {
    settings: postgresql_embedded::Settings,
    password_file: Utf8PathBuf,
    cache_dir: Utf8PathBuf,
    runtime_dir: Utf8PathBuf,
}

#[derive(Debug)]
struct TestConfig {
    version_req: &'static str,
    port: u16,
    password: &'static str,
    database_name: &'static str,
    table_sql: &'static str,
    insert_sql: &'static str,
    select_sql: &'static str,
    message: &'static str,
    base_dir: Utf8PathBuf,
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
}

impl TestConfig {
    fn new() -> Self {
        let base_dir = Utf8PathBuf::from(format!(
            "/var/tmp/pg-embedded-setup-it/e2e-{}",
            std::process::id()
        ));
        let install_dir = base_dir.join("install");
        let data_dir = base_dir.join("data");

        Self {
            version_req: "=16.4.0",
            port: 55_432,
            password: "diesel_pass",
            database_name: "diesel_demo",
            table_sql: "CREATE TABLE greetings (id SERIAL PRIMARY KEY, message TEXT NOT NULL)",
            insert_sql: "INSERT INTO greetings (message) VALUES ($1)",
            select_sql: "SELECT id, message FROM greetings ORDER BY id",
            message: "hello from diesel",
            base_dir,
            install_dir,
            data_dir,
        }
    }

    const fn base_dir(&self) -> &Utf8PathBuf {
        &self.base_dir
    }

    const fn install_dir(&self) -> &Utf8PathBuf {
        &self.install_dir
    }

    fn cache_dir(&self) -> Utf8PathBuf {
        self.install_dir.join("cache")
    }

    fn runtime_dir(&self) -> Utf8PathBuf {
        self.install_dir.join("run")
    }

    fn password_file(&self) -> Utf8PathBuf {
        self.install_dir.join(".pgpass")
    }

    fn bootstrap_env(&self) -> ScopedEnvVars {
        let port = self.port.to_string();

        build_env([
            ("PG_RUNTIME_DIR", self.install_dir.as_str()),
            ("PG_DATA_DIR", self.data_dir.as_str()),
            ("PG_VERSION_REQ", self.version_req),
            ("PG_PORT", port.as_str()),
            ("PG_SUPERUSER", "postgres"),
            ("PG_PASSWORD", self.password),
        ])
    }

    fn runtime_env(&self, password_file: &Utf8PathBuf) -> ScopedEnvVars {
        let cache_dir = self.cache_dir();
        let runtime_dir = self.runtime_dir();

        build_env([
            ("HOME", self.install_dir.as_str()),
            ("XDG_CACHE_HOME", cache_dir.as_str()),
            ("XDG_RUNTIME_DIR", runtime_dir.as_str()),
            ("PGPASSFILE", password_file.as_str()),
            ("TZDIR", "/usr/share/zoneinfo"),
            ("TZ", "UTC"),
        ])
    }
}

#[test]
fn e2e_postgresql_embedded_creates_and_queries_via_diesel() -> Result<()> {
    if !geteuid().is_root() {
        tracing::warn!("Skipping root-dependent PostgreSQL e2e test.");
        return Ok(());
    }

    let config = TestConfig::new();
    remove_tree(config.base_dir())?;

    let test_result = run_e2e_test(&config);
    clean_base_dir(&config, &test_result);

    test_result
}

fn run_e2e_test(config: &TestConfig) -> Result<()> {
    let Some(context) = bootstrap_postgres_environment(config)? else {
        return Ok(());
    };

    if std::env::var_os("PG_EMBEDDED_WORKER").is_some() {
        run_postgres_operations(config, &context)
    } else {
        tracing::warn!("Skipping diesel e2e: set PG_EMBEDDED_WORKER to exercise the worker path");
        Ok(())
    }
}

fn bootstrap_postgres_environment(config: &TestConfig) -> Result<Option<BootstrapContext>> {
    with_scoped_env(
        config.bootstrap_env(),
        || -> Result<Option<BootstrapContext>> {
            if let Err(err) = pg_embedded_setup_unpriv::run() {
                let message = err.to_string();
                if message.contains("rate limit exceeded") {
                    tracing::warn!("Skipping e2e postgres test: {message}");
                    return Ok(None);
                }
                return Err(err).wrap_err("initialise postgres environment");
            }

            let cfg = PgEnvCfg::load().wrap_err("reload pg settings from environment")?;
            let mut settings = cfg
                .to_settings()
                .wrap_err("convert environment to settings")?;
            if geteuid().is_root() {
                settings.temporary = false;
            }
            settings.timeout = Some(Duration::from_secs(60));

            let password_file = config.password_file();
            settings.password_file = password_file.clone().into_std_path_buf();
            config.password.clone_into(&mut settings.password);

            Ok(Some(BootstrapContext {
                settings,
                password_file,
                cache_dir: config.cache_dir(),
                runtime_dir: config.runtime_dir(),
            }))
        },
    )
}

fn run_postgres_operations(config: &TestConfig, context: &BootstrapContext) -> Result<()> {
    ensure_dir(&context.cache_dir, 0o755)?;
    ensure_dir(&context.runtime_dir, 0o755)?;
    provision_password_file_for_nobody(config, context)?;

    let env_vars = config.runtime_env(&context.password_file);
    let env_settings = context.settings.clone();

    with_scoped_env(env_vars, move || run_postgres_in_env(config, &env_settings))
}

fn provision_password_file_for_nobody(
    config: &TestConfig,
    context: &BootstrapContext,
) -> Result<()> {
    let install_handle =
        open_dir(config.install_dir()).context("open install directory for nobody")?;
    let password_relative = context
        .password_file
        .strip_prefix(config.install_dir())
        .map_err(|_| eyre!("password file must live inside install dir"))?;
    let mut pgpass = install_handle
        .open_with(
            password_relative.as_std_path(),
            OpenOptions::new().create(true).truncate(true).write(true),
        )
        .context("provision password file for embedded postgres")?;
    write!(pgpass, "{}", context.settings.password).context("write postgres password")?;
    pgpass.sync_all().context("flush password file")?;
    install_handle
        .set_permissions(
            password_relative.as_std_path(),
            cap_std::fs::Permissions::from_mode(0o600),
        )
        .context("set permissions on password file")?;
    let user = User::from_name("nobody")
        .context("resolve user 'nobody'")?
        .ok_or_else(|| eyre!("user 'nobody' not found"))?;
    fchown(&pgpass, Some(user.uid), Some(user.gid)).context("chown password file to nobody")?;
    Ok(())
}

fn run_diesel_operations(database_url: &str, config: &TestConfig) -> Result<()> {
    let mut connection = PgConnection::establish(database_url)
        .wrap_err("connect to embedded postgres via diesel")?;

    diesel::sql_query(config.table_sql)
        .execute(&mut connection)
        .wrap_err("create greetings table")?;

    diesel::sql_query(config.insert_sql)
        .bind::<Text, _>(config.message)
        .execute(&mut connection)
        .wrap_err("insert greeting row")?;

    let rows: Vec<GreetingRow> = diesel::sql_query(config.select_sql)
        .load(&mut connection)
        .wrap_err("select greetings via diesel")?;

    let expected = vec![GreetingRow {
        id: 1,
        message: config.message.to_owned(),
    }];
    ensure!(rows == expected, "unexpected query result: {rows:?}",);

    Ok(())
}

fn clean_base_dir(config: &TestConfig, result: &Result<()>) {
    if result.is_ok()
        && let Err(err) = remove_tree(config.base_dir())
    {
        tracing::warn!(
            "warning: failed to clean e2e base dir {}: {err}",
            config.base_dir().as_str()
        );
    }
}

fn run_postgres_in_env(
    config: &TestConfig,
    settings: &postgresql_embedded::Settings,
) -> Result<()> {
    tracing::info!(
        installation = %settings.installation_dir.display(),
        data = %settings.data_dir.display(),
        "postgresql directories configured",
    );
    let runtime = build_runtime()?;
    let (postgresql, database_url) = start_postgres(&runtime, settings, config)?;

    let run_result = (|| {
        ensure_database_exists(settings, config.database_name)?;
        run_diesel_operations(&database_url, config)?;
        Ok(())
    })();

    let stop_result = stop_postgres(&runtime, postgresql);
    run_result.and(stop_result)
}

fn build_runtime() -> Result<Runtime> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .wrap_err("initialise tokio runtime for postgresql-embedded e2e test")
}

fn start_postgres(
    runtime: &Runtime,
    settings: &postgresql_embedded::Settings,
    config: &TestConfig,
) -> Result<(PostgresHandle, String)> {
    if geteuid().is_root() {
        let worker = worker_from_env()?;
        let timeout = settings.timeout.unwrap_or(Duration::from_secs(60));
        run_worker_operation(worker.as_path(), settings, timeout, WorkerOperation::Setup)?;
        run_worker_operation(worker.as_path(), settings, timeout, WorkerOperation::Start)?;
        let database_url = settings.url(config.database_name);
        return Ok((
            PostgresHandle::Worker(WorkerHandle {
                worker,
                settings: settings.clone(),
                timeout,
            }),
            database_url,
        ));
    }

    runtime
        .block_on(async {
            let mut postgresql = PostgreSQL::new(settings.clone());
            postgresql.setup().await?;
            postgresql.start().await?;
            let database_url = postgresql.settings().url(config.database_name);
            Ok::<_, color_eyre::Report>((PostgresHandle::InProcess(postgresql), database_url))
        })
        .wrap_err("start embedded postgres via diesel helper")
}

fn stop_postgres(runtime: &Runtime, postgresql: PostgresHandle) -> Result<()> {
    match postgresql {
        PostgresHandle::InProcess(instance) => runtime
            .block_on(async { instance.stop().await })
            .wrap_err("stop embedded postgres instance"),
        PostgresHandle::Worker(handle) => run_worker_operation(
            handle.worker.as_path(),
            &handle.settings,
            handle.timeout,
            WorkerOperation::Stop,
        ),
    }
}
