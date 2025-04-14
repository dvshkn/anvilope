pub mod class;
pub mod classifier;
pub mod llm_completion;
pub mod db_table;
pub mod llm_job;
pub mod message;
pub mod meta;

pub use class::Class;
pub use classifier::Classifier;
use db_table::DbTable;
pub use llm_completion::LlmCompletion;
pub use llm_job::LlmJob;
pub use message::Message;
pub use meta::Meta;

use anyhow::Result;
use log::{
    debug,
    info,
};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Params;
use rusqlite::Row;
use std::collections::HashSet;

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

pub fn ensure_tables() {
    let conn = checkout_db_conn!();

    let mut expected_tables = HashSet::from([
        "class", "classifier", "llm_completion", "llm_job", "message", "meta",
    ]);
    let num_expected = expected_tables.len();

    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_schema WHERE type='table'
        AND name NOT LIKE 'sqlite_%'"
    ).unwrap();
    let mut name_iter = stmt.query_map([], |row| row.get::<_,String>(0)).unwrap();
    while let Some(Ok(table_name)) = name_iter.next() {
        expected_tables.remove(table_name.as_str());
    }

    if expected_tables.len() == num_expected {
        // none of the tables present, probably a fresh db
        info!("database is empty, creating tables");
        create_tables();
    } else if expected_tables.len() > 0 {
        panic!("some database tables missing, possible corruption or
            version mismatch");
    }
}

pub fn create_tables() {
    let conn = checkout_db_conn!();

    // tables without foreign keys
    conn.execute(Class::DDL, []).unwrap();
    conn.execute(Classifier::DDL, []).unwrap();
    conn.execute(Message::DDL, []).unwrap();
    conn.execute(Meta::DDL, []).unwrap();

    // tables with foreign keys
    conn.execute(LlmCompletion::DDL, []).unwrap();
    conn.execute(LlmJob::DDL, []).unwrap();
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
