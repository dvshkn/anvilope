use anyhow::Result;
use rusqlite::{named_params, Row};

use crate::checkout_db_conn;

#[derive(Debug, Clone)]
pub struct LlmCompletion {
    pub id: i64,
    pub message_id: i64,
    pub classifier_id: i64,
    pub llm_response: String,
    pub trace: String,
    pub class_id: Option<i64>,
    pub class_key: Option<String>,
    pub created_at: i64,
}

impl From<&Row<'_>> for LlmCompletion {
    fn from(r: &Row) -> Self {
        LlmCompletion {
            id: r.get("id").unwrap(),
            message_id: r.get("message_id").unwrap(),
            classifier_id: r.get("classifier_id").unwrap(),
            llm_response: r.get("llm_response").unwrap(),
            trace: r.get("trace").unwrap(),
            class_id: r.get("class_id").unwrap(),
            class_key: r.get("class_key").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl LlmCompletion {
    pub async fn create(message_id: i64, classifier_id: i64, llm_response: String,
        trace: String, class_id: Option<i64>, class_key: Option<String>)
        -> Result<LlmCompletion> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let res = super::exec_return_int2(&conn,
                "INSERT INTO llm_completion (message_id, classifier_id, llm_response,
                trace, class_id, class_key) VALUES (:message_id, :classifier_id, :llm_response,
                :trace, :class_id, :class_key) RETURNING id, created_at",
                named_params! {
                    ":message_id": message_id,
                    ":classifier_id": classifier_id,
                    ":llm_response": llm_response,
                    ":trace": trace,
                    ":class_id": class_id,
                    ":class_key": class_key,
                },
            );
            match res {
                Ok((id, created_at)) => {
                    Ok(LlmCompletion {
                        id,
                        message_id,
                        classifier_id,
                        llm_response,
                        trace,
                        class_id,
                        class_key,
                        created_at,
                    })
                },
                Err(err) => Err(err),
            }
        }).await?
    }
}
