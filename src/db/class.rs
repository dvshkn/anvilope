use anyhow::Result;
use rusqlite::{
    named_params,
    params_from_iter,
    Row,
};
use serde::{
    Deserialize,
    Serialize,
};

use crate::checkout_db_conn;
use super::DbTable;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Class {
    pub id: i64,
    pub key: String,
    pub prompt: String,
    pub archived_at: Option<i64>,
    pub created_at: i64,
}

impl DbTable for Class {
    const DDL: &'static str = "CREATE TABLE class (
        id INTEGER PRIMARY KEY,
        key TEXT NOT NULL,
        prompt TEXT NOT NULL,
        archived_at INTEGER,
        created_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL
    );
    CREATE INDEX idx_key ON class(key);
    CREATE INDEX idx_archived_at ON class(archived_at);";
}

impl From<&Row<'_>> for Class {
    fn from(r: &Row) -> Self {
        Class {
            id: r.get("id").unwrap(),
            key: r.get("key").unwrap(),
            prompt: r.get("prompt").unwrap(),
            archived_at: r.get("archived_at").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl Class {
    pub async fn create(key: String, prompt: String, archived: bool) -> Result<Class> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let created_at = super::timestamp();
            let archived_at = if archived { Some(created_at) } else { None };
            let res = super::exec_return_int(&conn,
                "INSERT INTO class (key, prompt, archived_at, created_at)
                VALUES (:key, :prompt, :archived_at, :created_at) RETURNING id",
                named_params! {
                    ":key": key,
                    ":prompt": prompt,
                    ":archived_at": archived_at,
                    ":created_at": created_at,
                },
            );
            match res {
                Ok(id) => {
                    Ok(Class { id, key, prompt, archived_at, created_at })
                },
                Err(err) => Err(err),
            }
        }).await?
    }

    pub async fn get_active() -> Result<Vec<Class>> {
        tokio::task::spawn_blocking(|| {
            let conn = checkout_db_conn!();
            super::exec_return_vec(&conn,
                "SELECT id, key, prompt, archived_at, created_at FROM
                class WHERE archived_at IS NULL",
                [],
            )
        }).await?
    }

    pub async fn set_active(ids: Vec<i64>) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let mut conn = checkout_db_conn!();
            let tx = conn.transaction()?;
            let now = super::timestamp();
            tx.execute(
                "UPDATE class SET archived_at = ? WHERE archived_at IS NULL",
                [&now],
            )?;
            let update_sql = format!(
                "UPDATE class SET archived_at = NULL WHERE id IN ({})",
                super::sql_param_list(ids.len())
            );
            tx.execute(update_sql.as_str(), params_from_iter(ids.iter()))?;
            tx.commit()?;
            Ok(())
        }).await?
    }
}
