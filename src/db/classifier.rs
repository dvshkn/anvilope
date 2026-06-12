use anyhow::Result;
use rusqlite::{
    named_params,
    Row
};

use crate::checkout_db_conn;

use super::DbTable;

#[derive(Debug, Clone)]
pub struct Classifier {
    pub id: i64,
    pub prompt: String,
    pub archived_at: Option<i64>,
    pub created_at: i64,
}

impl DbTable for Classifier {}

impl From<&Row<'_>> for Classifier {
    fn from(r: &Row) -> Self {
        Classifier {
            id: r.get("id").unwrap(),
            prompt: r.get("prompt").unwrap(),
            archived_at: r.get("archived_at").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl Classifier {
    pub async fn create(prompt: String, archived: bool) -> Result<Classifier> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let created_at = super::timestamp();
            let archived_at = if archived { Some(created_at) } else { None };
            let res = super::exec_return_int(&conn,
                "INSERT INTO classifier (prompt, archived_at, created_at)
                VALUES (:prompt, :archived_at, :created_at) RETURNING id",
                named_params! {
                    ":prompt": prompt,
                    ":archived_at": archived_at,
                    ":created_at": created_at,
                },
            );
            match res {
                Ok(id) => {
                    Ok(Classifier { id, prompt, archived_at, created_at })
                },
                Err(err) => Err(err),
            }
        }).await?
    }

    pub async fn get_active() -> Result<Option<Classifier>> {
        tokio::task::spawn_blocking(move || {
            super::exec_return_first(&checkout_db_conn!(),
                "SELECT id, prompt, archived_at, created_at FROM classifier
                WHERE archived_at IS NULL ORDER BY id DESC LIMIT 1",
                [],
            )
        }).await?
    }

    pub async fn set_active(id: i64) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let mut conn = checkout_db_conn!();
            let tx = conn.transaction()?;
            let now = super::timestamp();
            tx.execute(
                "UPDATE classifier SET archived_at = ? WHERE
                archived_at IS NULL",
                [&now],
            )?;
            tx.execute(
                "UPDATE classifier SET archived_at = NULL WHERE id = ?",
                [&id],
            )?;
            tx.commit()?;
            Ok(())
        }).await?
    }
}
