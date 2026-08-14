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
        (3, migrate_003_namespaced_classes as MigrationFn),
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

fn migrate_003_namespaced_classes(conn: &Connection) -> Result<()> {
    let contents = include_str!("../../migrations/migrate_003.sql");
    let _ = exec_sql_string(conn, contents)?;
    Ok(())
}

fn migrate_002_add_trace_column(conn: &Connection) -> Result<()> {
    let contents = include_str!("../../migrations/migrate_002.sql");
    let _ = exec_sql_string(conn, contents)?;
    Ok(())
}

fn migrate_001_baptize(conn: &Connection) -> Result<()> {
    let contents = include_str!("../../migrations/migrate_001.sql");
    let _ = exec_sql_string(conn, contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_sql(conn: &Connection, sql: &str) {
        exec_sql_string(conn, sql).unwrap();
    }

    fn schema_columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table)).unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    #[test]
    fn test_fresh_db_reaches_latest_schema() {
        let conn = Connection::open_in_memory().unwrap();
        run_sql(&conn, include_str!("../../migrations/seed_000.sql"));
        set_database_version(&conn, 0).unwrap();
        run_migrations(&conn).unwrap();

        assert_eq!(get_database_version(&conn).unwrap(), 3);
        assert!(schema_columns(&conn, "classifier").contains(&"key".to_string()));
        assert!(schema_columns(&conn, "class").contains(&"classifier_id".to_string()));
        assert!(schema_columns(&conn, "llm_job").contains(&"classifier_key".to_string()));
    }

    #[test]
    fn test_legacy_db_migration() {
        let conn = Connection::open_in_memory().unwrap();
        // bring the db up to the pre-migration-003 state
        run_sql(&conn, include_str!("../../migrations/seed_000.sql"));
        run_sql(&conn, include_str!("../../migrations/migrate_001.sql"));
        run_sql(&conn, include_str!("../../migrations/migrate_002.sql"));
        set_database_version(&conn, 2).unwrap();

        // legacy data from the old config format
        conn.execute(
            "INSERT INTO classifier (prompt, archived_at, created_at) VALUES ('p', NULL, 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO class (key, prompt, archived_at, created_at) VALUES ('spam', 'p', NULL, 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO message (account_key, class_key, from_address, from_name,
            subject, body_plain, body_html, uid, uid_validity)
            VALUES ('gmail', 'spam', 'a@b.c', 'A', 's', '', '', 1, 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO llm_completion (message_id, classifier_id, llm_response,
            trace, class_id, class_key) VALUES (1, 1, '{}', '[]', 1, 'spam')",
            [],
        ).unwrap();

        run_migrations(&conn).unwrap();

        let cf_key: String = conn.query_row(
            "SELECT key FROM classifier WHERE id = 1", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(cf_key, "default");

        let cf_id: i64 = conn.query_row(
            "SELECT id FROM classifier WHERE id = 1", [], |r| r.get(0)
        ).unwrap();
        let class_cf_id: i64 = conn.query_row(
            "SELECT classifier_id FROM class WHERE id = 1", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(class_cf_id, cf_id);

        let msg_class_key: String = conn.query_row(
            "SELECT class_key FROM message WHERE id = 1", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(msg_class_key, "default.spam");

        let comp_class_key: String = conn.query_row(
            "SELECT class_key FROM llm_completion WHERE id = 1", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(comp_class_key, "default.spam");

        assert!(schema_columns(&conn, "llm_job").contains(&"classifier_key".to_string()));
    }
}
