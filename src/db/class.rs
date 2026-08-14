use anyhow::Result;
use rusqlite::{
    named_params,
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
    pub classifier_id: i64,
    pub key: String,
    pub prompt: String,
    pub archived_at: Option<i64>,
    pub created_at: i64,
}

impl DbTable for Class {}

impl From<&Row<'_>> for Class {
    fn from(r: &Row) -> Self {
        Class {
            id: r.get("id").unwrap(),
            classifier_id: r.get("classifier_id").unwrap(),
            key: r.get("key").unwrap(),
            prompt: r.get("prompt").unwrap(),
            archived_at: r.get("archived_at").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl Class {
    pub async fn create(classifier_id: i64, key: String, prompt: String, archived: bool) -> Result<Class> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let created_at = super::timestamp();
            let archived_at = if archived { Some(created_at) } else { None };
            let res = super::exec_return_int(&conn,
                "INSERT INTO class (classifier_id, key, prompt, archived_at, created_at)
                VALUES (:classifier_id, :key, :prompt, :archived_at, :created_at) RETURNING id",
                named_params! {
                    ":classifier_id": classifier_id,
                    ":key": key,
                    ":prompt": prompt,
                    ":archived_at": archived_at,
                    ":created_at": created_at,
                },
            );
            match res {
                Ok(id) => {
                    Ok(Class { id, classifier_id, key, prompt, archived_at, created_at })
                },
                Err(err) => Err(err),
            }
        }).await?
    }

    pub async fn get_active() -> Result<Vec<Class>> {
        tokio::task::spawn_blocking(|| {
            let conn = checkout_db_conn!();
            super::exec_return_vec(&conn,
                "SELECT id, classifier_id, key, prompt, archived_at, created_at FROM
                class WHERE archived_at IS NULL",
                [],
            )
        }).await?
    }

    pub async fn archive(id: i64) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let now = super::timestamp();
            conn.execute(
                "UPDATE class SET archived_at = ? WHERE
                id = ? AND archived_at IS NULL",
                [&now, &id],
            )?;
            Ok(())
        }).await?
    }
}
