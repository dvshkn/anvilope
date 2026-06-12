use anyhow::Result;
use rusqlite::{
    named_params,
    Row,
};

use crate::checkout_db_conn;

use super::DbTable;

#[derive(Debug, Clone)]
pub struct Meta {
    pub id: i64,
    pub key: String,
    pub value: String,
    pub updated_at: i64,
}

impl DbTable for Meta {}

impl From<&Row<'_>> for Meta {
    fn from(r: &Row) -> Self {
        Meta {
            id: r.get("id").unwrap(),
            key: r.get("key").unwrap(),
            value: r.get("value").unwrap(),
            updated_at: r.get("updated_at").unwrap(),
        }
    }
}

impl Meta {
    pub async fn get(key: String) -> Result<Option<Meta>> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            super::exec_return_first(&conn,
                "SELECT id, key, value, updated_at FROM meta
                WHERE key = ? ORDER BY id ASC LIMIT 1",
                [&key]
            )
        }).await?
    }

    pub async fn set(key: String, value: String) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let updated_at = super::timestamp();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (:key, :value)
                ON CONFLICT (key) DO UPDATE SET value = :value,
                updated_at = :updated_at WHERE key = :key",
                named_params! {
                    ":key": key,
                    ":value": value,
                    ":updated_at": updated_at
                },
            )?;
            Ok(())
        }).await?
    }
}
