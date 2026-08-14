use anyhow::Result;
use rusqlite::{
    Row,
    named_params,
};

use crate::checkout_db_conn;

use super::DbTable;

#[derive(Debug, Clone)]
pub struct LlmJob {
    pub id: i64,
    pub message_id: i64,
    pub classifier_key: String,
    pub status: String,
    pub created_at: i64,
}

impl DbTable for LlmJob {}

impl From<&Row<'_>> for LlmJob {
    fn from(r: &Row) -> Self {
        LlmJob {
            id: r.get("id").unwrap(),
            message_id: r.get("message_id").unwrap(),
            classifier_key: r.get("classifier_key").unwrap(),
            status: r.get("status").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl LlmJob {
    pub const JOB_STATE_QUEUED: &'static str = "queued";
    pub const JOB_STATE_PROCESSING: &'static str = "processing";
    pub const JOB_STATE_COMPLETED: &'static str = "completed";
    pub const JOB_STATE_FAILED: &'static str = "failed";

    pub async fn enqueue(message_id: i64, classifier_key: &String) -> Result<i64> {
        let classifier_key = classifier_key.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let status = Self::JOB_STATE_QUEUED.to_string();
            super::exec_return_int(&conn,
                "INSERT INTO llm_job (message_id, classifier_key, status) VALUES
                (:message_id, :classifier_key, :status) RETURNING id",
                named_params! {
                    ":message_id": message_id,
                    ":classifier_key": classifier_key,
                    ":status": status,
                },
            )
        }).await?
    }

    pub async fn dequeue() -> Result<Option<LlmJob>> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let stmt_str = format!(
                "UPDATE llm_job SET status=\"{}\" WHERE
                id = (SELECT id FROM llm_job WHERE status=\"{}\" ORDER BY id
                ASC LIMIT 1) RETURNING id, message_id, classifier_key, status, created_at",
                LlmJob::JOB_STATE_PROCESSING,
                LlmJob::JOB_STATE_QUEUED,
            );
            super::exec_return_first(&conn,
                stmt_str.as_str(),
                [],
            )
        }).await?
    }

    pub async fn get(id: i64) -> Result<Option<LlmJob>> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            super::exec_return_first(&conn,
                "SELECT id, message_id, classifier_key, status, created_at
                FROM llm_job WHERE id = ?",
                [&id],
            )
        }).await?
    }

    pub async fn set_status(id: i64, status: &'static str) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            conn.execute(
                "UPDATE llm_job SET status=? WHERE id = ?",
                (status, &id),
            )?;
            Ok(())
        }).await?
    }
}
