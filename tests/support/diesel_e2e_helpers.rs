//! Helpers for the Diesel-based `PostgreSQL` embedded e2e test.

use std::path::PathBuf;
use std::time::Duration;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Result, eyre};
use diesel::prelude::*;
use diesel::sql_types::{Bool, Text};
use pg_embedded_setup_unpriv::worker_process_test_api::{
    WorkerOperation, WorkerRequest, WorkerRequestArgs, run as run_worker,
};
use postgresql_embedded::PostgreSQL;

#[derive(QueryableByName, Debug)]
struct DatabaseExists {
    #[diesel(sql_type = Bool)]
    exists: bool,
}

#[derive(Debug)]
pub(crate) enum PostgresHandle {
    InProcess(PostgreSQL),
    Worker(WorkerHandle),
}

#[derive(Debug)]
pub(crate) struct WorkerHandle {
    pub(crate) worker: Utf8PathBuf,
    pub(crate) settings: postgresql_embedded::Settings,
    pub(crate) timeout: Duration,
}

pub(crate) fn ensure_database_exists(
    settings: &postgresql_embedded::Settings,
    database_name: &str,
) -> Result<()> {
    let admin_url = settings.url("postgres");
    let mut connection =
        PgConnection::establish(&admin_url).wrap_err("connect to admin database")?;
    let exists: DatabaseExists =
        diesel::sql_query("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1) AS exists")
            .bind::<Text, _>(database_name)
            .get_result(&mut connection)
            .wrap_err("check database existence")?;
    if !exists.exists {
        let create_statement = format!("CREATE DATABASE {}", quote_identifier(database_name));
        diesel::sql_query(create_statement)
            .execute(&mut connection)
            .wrap_err("create database")?;
    }
    Ok(())
}

pub(crate) fn worker_from_env() -> Result<Utf8PathBuf> {
    let worker = std::env::var_os("PG_EMBEDDED_WORKER")
        .ok_or_else(|| eyre!("PG_EMBEDDED_WORKER must be set for worker runs"))?;
    let path = PathBuf::from(worker);
    Utf8PathBuf::from_path_buf(path).map_err(|_| eyre!("PG_EMBEDDED_WORKER must be UTF-8"))
}

pub(crate) fn run_worker_operation(
    worker: &camino::Utf8Path,
    settings: &postgresql_embedded::Settings,
    timeout: Duration,
    operation: WorkerOperation,
) -> Result<()> {
    let env_vars = Vec::new();
    let args = WorkerRequestArgs {
        worker,
        settings,
        env_vars: &env_vars,
        operation,
        timeout,
    };
    let request = WorkerRequest::new(args);
    run_worker(&request)
        .map_err(|err| eyre!(err))
        .wrap_err(format!("worker operation {} failed", operation.as_str()))
}

fn quote_identifier(identifier: &str) -> String {
    let mut quoted = String::with_capacity(identifier.len() + 2);
    quoted.push('"');
    for ch in identifier.chars() {
        if ch == '"' {
            quoted.push('"');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}
