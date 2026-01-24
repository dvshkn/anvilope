use anyhow::Result;
use rusqlite::{named_params, Row};

use crate::checkout_db_conn;

use super::DbTable;

#[derive(Debug, Clone)]
pub struct LlmCompletion {
    pub id: i64,
    pub message_id: i64,
    pub classifier_id: i64,
    pub llm_response: String,
    pub endpoint_response: String,
    pub class_id: Option<i64>,
    pub class_key: Option<String>,
    pub created_at: i64,
}

// TODO: remove endpoint_response column

impl DbTable for LlmCompletion {
    const DDL: &'static str = "CREATE TABLE llm_completion (
        id INTEGER PRIMARY KEY,
        message_id INTEGER NOT NULL,
        classifier_id INTEGER NOT NULL,
        llm_response TEXT NOT NULL,
        endpoint_response TEXT NOT NULL,
        class_id INTEGER,
        class_key TEXT,
        created_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL,
        FOREIGN KEY(message_id) REFERENCES message(id),
        FOREIGN KEY(classifier_id) REFERENCES classifier(id),
        FOREIGN KEY(class_id) REFERENCES class(id)
    )";
}

impl From<&Row<'_>> for LlmCompletion {
    fn from(r: &Row) -> Self {
        LlmCompletion {
            id: r.get("id").unwrap(),
            message_id: r.get("message_id").unwrap(),
            classifier_id: r.get("classifier_id").unwrap(),
            llm_response: r.get("llm_response").unwrap(),
            endpoint_response: r.get("endpoint_response").unwrap(),
            class_id: r.get("class_id").unwrap(),
            class_key: r.get("class_key").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl LlmCompletion {
    pub async fn create(message_id: i64, classifier_id: i64, llm_response: String,
        endpoint_response: String, class_id: Option<i64>, class_key: Option<String>)
        -> Result<LlmCompletion> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let res = super::exec_return_int2(&conn,
                "INSERT INTO llm_completion (message_id, classifier_id, llm_response,
                endpoint_response, class_id, class_key) VALUES (:message_id, :classifier_id, :llm_response,
                :endpoint_response, :class_id, :class_key) RETURNING id, created_at",
                named_params! {
                    ":message_id": message_id,
                    ":classifier_id": classifier_id,
                    ":llm_response": llm_response,
                    ":endpoint_response": endpoint_response,
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
                        endpoint_response,
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
