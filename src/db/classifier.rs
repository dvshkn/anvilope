use anyhow::Result;
use rusqlite::{
    named_params,
    params_from_iter,
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

    pub async fn get_active(key: &String) -> Result<Option<Classifier>> {
        let owned_key = key.to_owned();
        tokio::task::spawn_blocking(move || {
            super::exec_return_first(&checkout_db_conn!(),
                "SELECT id, key, prompt, archived_at, created_at FROM classifier
                WHERE archived_at IS NULL AND key = ? ORDER BY id DESC LIMIT 1",
                [&owned_key],
            )
        }).await?
    }

    pub async fn set_active(ids: Vec<i64>) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let mut conn = checkout_db_conn!();
            let tx = conn.transaction()?;
            let now = super::timestamp();
            tx.execute(
                "UPDATE classifier SET archived_at = ? WHERE archived_at IS NULL",
                [&now],
            )?;
            let update_sql = format!(
                "UPDATE classifier SET archived_at = NULL WHERE id IN ({})",
                super::sql_param_list(ids.len())
            );
            tx.execute(update_sql.as_str(), params_from_iter(ids.iter()))?;
            tx.commit()?;
            Ok(())
        }).await?
    }
}
