pub mod classifier;
pub mod llm_completion;
pub mod db_table;
pub mod llm_job;
pub mod message;
pub mod meta;
pub mod migrate;

pub use classifier::Classifier;
use db_table::DbTable;
pub use llm_completion::LlmCompletion;
pub use llm_job::LlmJob;
pub use message::Message;
pub use meta::Meta;

use anyhow::Result;
use log::{
    debug,
};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Params;
use rusqlite::Row;

use crate::anvilope_config;
use crate::checkout_db_conn;

pub fn init_pool() -> Pool<SqliteConnectionManager> {
    let db_path = anvilope_config!().db_path.to_owned();
    let db_path_2 = db_path.clone();
    let manager = SqliteConnectionManager::file(db_path);
    let pool = Pool::new(manager).unwrap();
    debug!("database pool initialized: {:#?}", db_path_2);

    pool
}

pub fn ensure_tables() -> Result<()> {
    let conn = checkout_db_conn!();

    let num_tables = exec_return_int(
        &conn,
        "SELECT count(name) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        []
    )?;

    if num_tables <= 0 {
        // none of the tables present, probably a fresh db
        migrate::seed_database(&conn)?;
    }
    migrate::run_migrations(&conn)?;

    Ok(())
}

pub fn exec_return_first<T, P>(conn: &Connection, statement_str: &str, params: P) -> Result<Option<T>>
where
    T: DbTable + for<'a> From<&'a Row<'a>>,
    P: Params,
{
    let mut stmt = conn.prepare(statement_str)?;
    let row = stmt.query_row(params, T::to_struct).optional()?;
    Ok(row)
}

pub fn exec_return_vec<T, P>(conn: &Connection, statement_str: &str, params: P) -> Result<Vec<T>>
where
    T: DbTable + for<'a> From<&'a Row<'a>>,
    P: Params,
{
    let mut stmt = conn.prepare(statement_str)?;
    let rows = T::to_vec(stmt.query(params))?;
    Ok(rows)
}

pub fn exec_return_int<P>(conn: &Connection, statement_str: &str, params: P) -> Result<i64>
where
    P: Params,
{
    let mut stmt = conn.prepare(statement_str)?;
    let int: i64 = stmt.query_row(params, |row| row.get(0))?;
    Ok(int)
}

pub fn exec_return_int2<P>(conn: &Connection, statement_str: &str, params: P) -> Result<(i64, i64)>
where
    P: Params,
{
    let mut stmt = conn.prepare(statement_str)?;
    let int2: (i64, i64) = stmt.query_row(params, |row| {
        Ok((row.get(0).unwrap(), row.get(1).unwrap()))
    })?;
    Ok(int2)
}

pub fn timestamp() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn sql_param_list(param_count: usize) -> String {
    let mut result = "?,".repeat(param_count);
    result.pop();
    result
}
