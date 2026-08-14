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
    pub key: String,
    pub prompt: String,
    pub archived_at: Option<i64>,
    pub created_at: i64,
}

impl DbTable for Classifier {}

impl From<&Row<'_>> for Classifier {
    fn from(r: &Row) -> Self {
        Classifier {
            id: r.get("id").unwrap(),
            key: r.get("key").unwrap(),
            prompt: r.get("prompt").unwrap(),
            archived_at: r.get("archived_at").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl Classifier {
    pub async fn create(key: String, prompt: String, archived: bool) -> Result<Classifier> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let created_at = super::timestamp();
            let archived_at = if archived { Some(created_at) } else { None };
            let res = super::exec_return_int(&conn,
                "INSERT INTO classifier (key, prompt, archived_at, created_at)
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
                    Ok(Classifier { id, key, prompt, archived_at, created_at })
                },
                Err(err) => Err(err),
            }
        }).await?
    }

    // multiple classifiers can be active at the same time
    pub async fn get_active() -> Result<Vec<Classifier>> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            super::exec_return_vec(&conn,
                "SELECT id, key, prompt, archived_at, created_at FROM classifier
                WHERE archived_at IS NULL",
                [],
            )
        }).await?
    }

    pub async fn archive(id: i64) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let now = super::timestamp();
            conn.execute(
                "UPDATE classifier SET archived_at = ? WHERE
                id = ? AND archived_at IS NULL",
                [&now, &id],
            )?;
            Ok(())
        }).await?
    }
}
