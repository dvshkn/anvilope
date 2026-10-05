use anyhow::Result;
use rusqlite::{
    named_params,
    Row
};
use std::collections::HashMap;

use crate::{
    checkout_db_conn,
    config,
};

use super::DbTable;

#[derive(Debug, Clone)]
pub struct Classifier {
    pub id: i64,
    pub key: String,
    pub prompt: String,
    pub classes: String, // json
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
            classes: r.get("classes").unwrap(),
            archived_at: r.get("archived_at").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl Classifier {
    pub async fn create(
        key: String,
        prompt: String,
        classes: &HashMap<String, String>,
        archived: bool
    ) -> Result<Classifier> {
        let classes_json = serde_json::to_string(classes)?;
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let created_at = super::timestamp();
            let archived_at = if archived { Some(created_at) } else { None };
            let res = super::exec_return_int(&conn,
                "INSERT INTO classifier (key, prompt, classes, archived_at, created_at)
                VALUES (:key, :prompt, :classes, :archived_at, :created_at) RETURNING id",
                named_params! {
                    ":key": key,
                    ":prompt": prompt,
                    ":classes": classes_json,
                    ":archived_at": archived_at,
                    ":created_at": created_at,
                },
            );
            match res {
                Ok(id) => {
                    Ok(Classifier {
                        id,
                        key,
                        prompt,
                        classes: classes_json,
                        archived_at,
                        created_at
                    })
                },
                Err(err) => Err(err),
            }
        }).await?
    }

    pub async fn get_active() -> Result<HashMap<String, Classifier>> {
        tokio::task::spawn_blocking(move || {
            let mut result_map = HashMap::<String, Classifier>::new();
            let result_query: Result<Vec<Classifier>> = super::exec_return_vec(&checkout_db_conn!(),
                "SELECT id, key, prompt, classes, archived_at, created_at
                FROM classifier WHERE archived_at IS NULL",
                [],
            );
            match result_query {
                Ok(rows) => {
                    rows.iter().for_each(|classifier| {
                        result_map.insert(classifier.key.to_owned(), classifier.to_owned());
                    });
                    Ok(result_map)
                },
                Err(err) => Err(err),
            }
        }).await?
    }

    pub async fn set_active(ids: &Vec<i64>) -> Result<()> {
        let id_list_string = ids.iter().map(|i| i.to_string())
            .collect::<Vec<String>>()
            .join(",");
        tokio::task::spawn_blocking(move || {
            let mut conn = checkout_db_conn!();
            let tx = conn.transaction()?;
            let now = super::timestamp();
            tx.execute(
                "UPDATE classifier SET archived_at = ? WHERE
                archived_at IS NULL",
                [&now],
            )?;
            let statement = format!(
                "UPDATE classifier SET archived_at = NULL WHERE id in ({})",
                id_list_string
            );
            tx.execute(statement.as_ref(), [])?;
            tx.commit()?;
            Ok(())
        }).await?
    }

    pub fn equivalent_to(&self, config: &config::Classifier) -> bool {
        let self_classes = serde_json::from_str::<HashMap<String,String>>(self.classes.as_ref());
        if self_classes.is_err() {
            return false;
        }
        let self_classes = self_classes.unwrap();

        if self.prompt != config.prompt {
            return false;
        }
        if self_classes.keys().count() != config.classes.keys().count() {
            return false;
        }

        for (self_class_k, self_class_prompt) in self_classes {
            if let Some(config_class_prompt) = config.classes.get(&self_class_k) {
                if self_class_prompt != *config_class_prompt {
                    return false;
                }
            } else {
                return false;
            }
        }

        return true;
    }
}
