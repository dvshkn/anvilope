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
pub struct Message {
    pub id: i64,
    pub account_key: String,
    pub class_key: Option<String>,
    pub from_address: String,
    pub from_name: String,
    pub subject: String,
    pub body_plain: String,
    pub body_html: String,
    pub uid: u32,
    pub uid_validity: u32,
    pub created_at: i64,
}

impl DbTable for Message {
    const DDL: &'static str = "CREATE TABLE message (
        id INTEGER PRIMARY KEY,
        account_key TEXT NOT NULL,
        class_key TEXT,
        from_address TEXT NOT NULL,
        from_name TEXT NOT NULL,
        subject TEXT NOT NULL,
        body_plain TEXT NOT NULL,
        body_html TEXT NOT NULL,
        uid INTEGER NOT NULL,
        uid_validity INTEGER NOT NULL,
        created_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL
    );
    CREATE INDEX idx_account_key ON message(account_key);
    CREATE INDEX idx_class_key ON message(class_key);";
}

impl From<&Row<'_>> for Message {
    fn from(r: &Row) -> Self {
        Message {
            id: r.get("id").unwrap(),
            account_key: r.get("account_key").unwrap(),
            class_key: r.get("class_key").unwrap(),
            from_address: r.get("from_address").unwrap(),
            from_name: r.get("from_name").unwrap(),
            subject: r.get("subject").unwrap(),
            body_plain: r.get("body_plain").unwrap(),
            body_html: r.get("body_html").unwrap(),
            uid: r.get("uid").unwrap(),
            uid_validity: r.get("uid_validity").unwrap(),
            created_at: r.get("created_at").unwrap(),
        }
    }
}

impl Message {
    pub async fn create(mut msg: Message) -> Result<Message> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            let res = super::exec_return_int2(&conn,
                "INSERT INTO message (account_key, class_key, from_address,
                from_name, subject, body_plain, body_html, uid, uid_validity)
                VALUES (:account_key, :class_key, :from_address, :from_name,
                :subject, :body_plain, :body_html, :uid, :uid_validity)
                RETURNING id, created_at",
                named_params! {
                    ":account_key": msg.account_key,
                    ":class_key": msg.class_key,
                    ":from_address": msg.from_address,
                    ":from_name": msg.from_name,
                    ":subject": msg.subject,
                    ":body_plain": msg.body_plain,
                    ":body_html": msg.body_html,
                    ":uid": msg.uid,
                    ":uid_validity": msg.uid_validity,
                }
            );
            match res {
                Ok((id, created_at)) => {
                    msg.id = id;
                    msg.created_at = created_at;
                    Ok(msg)
                },
                Err(err) => Err(err),
            }
        }).await?
    }

    pub async fn get(id: i64) -> Result<Option<Message>> {
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            super::exec_return_first(&conn,
                "SELECT * FROM message WHERE id = ?",
                [&id],
            )
        }).await?
    }

    pub async fn get_by_class(class_key: &String) -> Result<Vec<Message>> {
        let owned_class_key = class_key.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            super::exec_return_vec(&conn,
                "SELECT * FROM message WHERE class_key = ?",
                [&owned_class_key])
        }).await?
    }

    pub async fn set_class(id: i64, class_key: &String) -> Result<Option<Message>> {
        let owned_class_key = class_key.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = checkout_db_conn!();
            super::exec_return_first(&conn,
                "UPDATE message SET class_key = ? WHERE id = ? RETURNING *",
                (&owned_class_key, &id)
            )
        }).await?
    }
}
