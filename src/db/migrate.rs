use anyhow::Result;
use fallible_iterator::FallibleIterator;
use log::{
    debug,
    info,
};
use rusqlite::{
    Batch,
    Connection
};

type MigrationFn = fn(conn: &Connection) -> Result<()>;

pub fn run_migrations(conn: &Connection) -> Result<()> {
    // add entries to this array to register a new database migration
    let mut migrations = [
        (1, migrate_001_baptize as MigrationFn),
        (2, migrate_002_add_trace_column as MigrationFn),
        (3, migrate_003_add_classifier_key as MigrationFn),
    ];
    migrations.sort_by_key(|tup| tup.0);

    let curr_version = get_database_version(conn)?;
    let latest_version = migrations.last().unwrap().0;
    if latest_version > curr_version {
        // TODO: create backup before migrating

        info!("migrating database from version {} to {}", curr_version, latest_version);
        let new_migrations = migrations.into_iter().filter(|tup| tup.0 > curr_version);

        for (version, migration_fn) in new_migrations {
            debug!("========== db::migrate v{} start ==========", version);
            migration_fn(&conn)?;
            set_database_version(conn, version)?;
            debug!("=========== db::migrate v{} end ===========", version);
        }
    } else {
        info!("database schema is up to date");
    }

    Ok(())
}

pub fn seed_database(conn: &Connection) -> Result<()> {
    // update both of these values to register a new database seed
    let latest_contents = include_str!("../../migrations/seed_000.sql");
    let latest_version = 0;

    info!("creating new database");

    debug!("========== db::migrate seed start ==========");
    exec_sql_string(conn, latest_contents)?;
    set_database_version(conn, latest_version)?;
    debug!("=========== db::migrate seed end ===========");

    Ok(())
}

//
// utils
///////////

pub fn get_database_version(conn: &Connection) -> Result<i32> {
    let mut stmt = conn.prepare("PRAGMA user_version")?;
    Ok(stmt.query_row({}, |row| row.get(0))?)
}

pub fn set_database_version(conn: &Connection, v: i32) -> Result<()> {
    conn.execute(format!("PRAGMA user_version = {}", v).as_str(), [])?;
    Ok(())
}

pub fn exec_sql_string(conn: &Connection, statements: &str) -> Result<usize> {
    let allowed_errors = [rusqlite::Error::ExecuteReturnedResults, rusqlite::Error::QueryReturnedNoRows];
    let mut batch = Batch::new(conn, statements);
    let mut count = 0;
    while let Some(mut stmt) = batch.next()? {
        if let Some(sql) = stmt.expanded_sql() {
            debug!("{}", sql.trim());
        }
        if let Err(err) = stmt.execute([]) {
            if !allowed_errors.contains(&err) {
                return Err(anyhow::anyhow!(err));
            }
        }
        count += 1;
    }
    Ok(count)
}

//
// migrations
////////////////

fn migrate_002_add_trace_column(conn: &Connection) -> Result<()> {
    let contents = include_str!("../../migrations/migrate_002.sql");
    let _ = exec_sql_string(conn, contents)?;
    Ok(())
}

fn migrate_003_add_classifier_key(conn: &Connection) -> Result<()> {
    let contents = include_str!("../../migrations/migrate_003.sql");
    let _ = exec_sql_string(conn, contents)?;
    Ok(())
}

fn migrate_001_baptize(conn: &Connection) -> Result<()> {
    let contents = include_str!("../../migrations/migrate_001.sql");
    let _ = exec_sql_string(conn, contents)?;
    Ok(())
}
