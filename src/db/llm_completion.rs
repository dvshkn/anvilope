use anyhow::Result;
use rusqlite::{named_params, Row};

use crate::checkout_db_conn;

#[derive(Debug, Clone)]
pub struct LlmCompletion {
    pub id: i64,
    pub message_id: i64,
    pub llm_response: String,
    pub trace: String,
    pub classifier_key: String,
    pub class_key: Option<String>,
    pub created_at: i64,
}

impl From<&Row<'_>> for LlmCompletion {
    fn from(r: &Row) -> Self {
        LlmCompletion {
            id: r.get("id").unwrap(),
            message_id: r.get("message_id").unwrap(),
            llm_response: r.get("llm_response").unwrap(),
            trace: r.get("trace").unwrap(),
            classifier_key: r.get("classifier_key").unwrap(),
            class_key: r.get("class_key").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl LlmCompletion {
    pub async fn create(message_id: i64, llm_response: String, trace: String,
        classifier_key: String, class_key: Option<String>)
        -> Result<LlmCompletion> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let res = super::exec_return_int2(&conn,
                "INSERT INTO llm_completion (message_id, llm_response, trace,
                classifier_key, class_key) VALUES (:message_id, :llm_response,
                :trace, :classifier_key, :class_key) RETURNING id, created_at",
                named_params! {
                    ":message_id": message_id,
                    ":llm_response": llm_response,
                    ":trace": trace,
                    ":classifier_key": classifier_key,
                    ":class_key": class_key,
                },
            );
            match res {
                Ok((id, created_at)) => {
                    Ok(LlmCompletion {
                        id,
                        message_id,
                        llm_response,
                        trace,
                        classifier_key,
                        class_key,
                        created_at,
                    })
                },
                Err(err) => Err(err),
            }
        }).await?
    }
}
