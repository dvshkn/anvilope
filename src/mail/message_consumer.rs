use anyhow::Result;
use async_imap::Session;
use chrono::DateTime;
use futures::TryStreamExt;
use log::debug;
use log::error;
use regex::Regex;
use tokio::sync::mpsc::Sender;

use crate::config::Account;
use crate::db::Message;
use crate::AnvilopeEvent;

use super::AnvilopeTlsStream;

#[derive(PartialEq)]
pub enum MessageConsumerEvent {
    Quit,
    NewMessage { mailbox: String, ord: i64 },
}

pub struct MessageConsumer {
    pub account_key: String,
    pub session: Session<AnvilopeTlsStream>,
    pub root_tx: Sender<AnvilopeEvent>,
}

impl MessageConsumer {
    pub async fn new(account: &Account, root_tx: Sender<AnvilopeEvent>)
    -> Result<MessageConsumer> {
        let session = super::new_imap_session(account).await?;
        Ok(MessageConsumer { account_key: account._key.to_owned(), session, root_tx })
    }

    pub async fn download_message(&mut self, mailbox: String, ord: u32) -> () {
        let result = self.download_message_inner(mailbox, ord).await;
        match result {
            Ok(event) => {
                self.root_tx.send(event).await.unwrap();
            }
            Err(err) => {
                error!("download_message error: {:#?}", err);
            }
        }
    }

    pub async fn download_message_inner(&mut self, mailbox: String, ord: u32) -> Result<AnvilopeEvent> {
        self.session.select(mailbox).await?;

        let stream = self.session.fetch(
            ord.to_string(),
            "(UID RFC822.SIZE BODY.PEEK[HEADER] BODY.PEEK[TEXT])",
        ).await?;
        let msgs: Vec<_> = stream.try_collect().await?;
        let msg = msgs.first().expect("no messages!");

        let header = msg.header().expect("no header!");
        let body = msg.text().expect("no body!");
        let body = std::str::from_utf8(body).expect("body not UTF-8").to_string();

        let mut db_record = parse_header(&self.account_key, &header)?;
        db_record.body = body;
        let db_record = Message::create(db_record).await?;

        debug!("database message record: {}\n{:#?}", db_record.id, db_record);

        Ok(AnvilopeEvent::MessageDownloaded { message_id: db_record.id })
    }
}

fn parse_header(account_key: &String, bytes: &[u8]) -> Result<Message> {
    let header = std::str::from_utf8(bytes)?;

    let mut from_address = "".to_string();
    let mut from_name = "".to_string();
    let mut subject = "".to_string();
    let mut date: i64 = 0;
    for line in header.lines() {
        if line.starts_with("From: ") {
            let from = line.strip_prefix("From: ").unwrap();
            (from_address, from_name) = parse_from_field(from);
            println!("from value = {}", from);
        } else if line.starts_with("Subject: ") {
            subject = line.strip_prefix("Subject: ").unwrap().to_string();
            println!("subject value = {}", subject);
        } else if line.starts_with("Date: ") {
            let date_str = line.strip_prefix("Date: ").unwrap();
            date = parse_date_field(date_str).unwrap();
            println!("date value = {}", date);
        }
    }

    Ok(Message {
        id: 0,
        account_key: account_key.to_owned(),
        class_key: None,
        from_address,
        from_name,
        subject,
        body: "".to_string(),
        created_at: date,
    })
}

fn parse_from_field(from: &str) -> (String, String) {
    let mut email = "".to_string();
    let mut name = "".to_string();

    let re_addr = Regex::new(r"<(?P<email>[^@\s]+@[^@\s]+\.[^@\s]+)>").unwrap();

    if let Some(vals) = re_addr.captures(from) {
        email = (&vals["email"]).to_string();
        name = from.replace(&format!("<{}>", email), "");
        name = name.trim().to_string();
        if let Some(s) = name.as_str().strip_prefix("\"") {
            name = s.to_string();
        }
        if let Some(s) = name.as_str().strip_suffix("\"") {
            name = s.to_string();
        }
    }

    // TODO: support ancient format "email@address.com (Full Name)"

    (email, name)
}

fn parse_date_field(date: &str) -> Option<i64> {
    if let Ok(dt) = DateTime::parse_from_rfc2822(date) {
        return Some(dt.timestamp_millis());
    }
    None
}
