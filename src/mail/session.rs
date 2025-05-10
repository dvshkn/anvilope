use anyhow::{
    anyhow,
    Error,
    Result,
};
use async_imap::Session;
use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::MailboxDatum;
use async_imap::imap_proto::types::Response;
use async_imap::types::Capability;
use chrono::{
    DateTime,
    Utc,
};
use futures::TryStreamExt;
use log::{
    debug,
    error,
    info,
};
use tokio::sync::Mutex;
use std::cell::Cell;
use std::io;
use std::time::Duration;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

use crate::AnvilopeEvent;
use crate::config::Account;
use crate::db::Message;

use super::AnvilopeTlsStream;
use super::parse;

// TODO: make this configurable because certain firewalls might require a lower
//       duration to keep connection alive
const IDLE_DURATION: Duration = Duration::from_secs(20*60);

// do not attempt to reconnect unless previous session was able to stay alive
// for at least this long
const ATTEMPT_RECONNECT_DURATION: Duration = Duration::from_secs(30*60);

#[derive(PartialEq)]
pub enum ImapSessionManagerEvent {
    Quit,
    Data,
    Timeout,
}

pub struct ImapSessionManager {
    pub account_key: String,
    // Option type is to handle how async-imap essentially destroys the session
    // object when entering idle state and gives you back a new one when leaving
    // idle
    pub session_cell: Cell<Option<Session<AnvilopeTlsStream>>>,
    pub root_tx: Sender<AnvilopeEvent>,
    pub connected_at: DateTime<Utc>,
}

impl ImapSessionManager {
    pub async fn new(account: &Account,
                     root_tx: Sender<AnvilopeEvent>) -> Result<ImapSessionManager> {
        info!("connecting to {}", account.host);

        let mut session = super::connect(&account).await?;

        let caps = session.capabilities().await?;
        let idle_support = caps.iter().any(|c| {
            if let Capability::Atom(s) = c {
                s == "IDLE"
            } else {
                false
            }
        });
        if !idle_support {
            session.logout().await?;
            return Err(Error::msg("server does not support IDLE"));
        }

        info!("selecting mailbox INBOX");
        session.select("INBOX").await?;
        let session_cell = Cell::new(Some(session));
        let connected_at = Utc::now();

        Ok(ImapSessionManager {
            account_key: account._key.to_owned(),
            root_tx,
            session_cell,
            connected_at
        })
    }

    pub fn should_attempt_reconnect(&self) -> bool {
        let delta = Utc::now() - self.connected_at;
        delta.to_std().unwrap() > ATTEMPT_RECONNECT_DURATION
    }

    pub async fn logout(&self) -> Result<()> {
        let mut session = self.session_cell.replace(None)
            .ok_or_else(|| anyhow!("expected active IMAP session, found None"))?;
        session.logout().await?;
        Ok(())
    }

    pub fn spawn_listener(listener_ref: Arc<Mutex<ImapSessionManager>>) -> () {
        tokio::spawn(async move {
            let mut local_listener = listener_ref.lock().await;
            let result = local_listener.listen_for_messages().await;
            match result {
                Ok(event) => {
                    local_listener.root_tx.send(event).await.unwrap();
                }
                Err(err) => {
                    if let Some(io_err) = err.downcast_ref::<io::Error>() {
                        if io_err.kind() == io::ErrorKind::UnexpectedEof {
                            // abrupt disconnect, gmail seems to do this one after 24h
                            let event = AnvilopeEvent::AccountDisconnect {
                                account_key: local_listener.account_key.to_owned()
                            };
                            local_listener.root_tx.send(event).await.unwrap();
                            // start the listen loop again
                            let event = AnvilopeEvent::AccountIdleTimeout {
                                account_key: local_listener.account_key.to_owned(),
                            };
                            local_listener.root_tx.send(event).await.unwrap();
                        }
                    }
                    error!("listen_for_messages error: {:#?}", err);
                }
            };
        });
    }

    async fn listen_for_messages(&mut self) -> Result<AnvilopeEvent> {
        info!("entering idle state");

        let session = self.session_cell.replace(None)
            .ok_or_else(|| anyhow!("expected active IMAP session, found None"))?;
        let mut idle_handle = session.idle();
        idle_handle.init().await?;

        // TODO: use interrupt token to logout from server gracefully
        let (idle_wait, _interrupt) = idle_handle.wait_with_timeout(IDLE_DURATION);

        let result = match idle_wait.await? {
            IdleResponse::NewData(data) => {
                if let Response::MailboxData(MailboxDatum::Exists(ordinal)) = data.parsed() {
                    let ord = *ordinal;
                    info!("new message! ord = {}", ord);
                    Ok(AnvilopeEvent::AccountNewMessage {
                        account_key: self.account_key.to_owned(),
                        mailbox: "INBOX".to_string(),
                        ord,
                    })
                } else {
                    // other activity happening in the mailbox, resume idle
                    debug!("other new data received, resuming idle...");
                    let account_key = self.account_key.to_owned();
                    Ok(AnvilopeEvent::AccountIdleTimeout { account_key })
                }
            }
            IdleResponse::ManualInterrupt => {
                let account_key = self.account_key.to_owned();
                Ok(AnvilopeEvent::AccountIdleTimeout { account_key })
            }
            IdleResponse::Timeout => {
                let account_key = self.account_key.to_owned();
                Ok(AnvilopeEvent::AccountIdleTimeout { account_key })
            }
        };

        // be polite, always put the session back
        let session = idle_handle.done().await?;
        self.session_cell.replace(Some(session));

        result
    }

    pub async fn download_message(listener_ref: Arc<Mutex<ImapSessionManager>>, mailbox: String, ord: u32) -> () {
        let mut local_listener = listener_ref.lock().await;
        let result = local_listener.download_message_inner(&mailbox, &ord).await;
        match result {
            Ok(event) => {
                local_listener.root_tx.send(event).await.unwrap();
            }
            Err(err) => {
                if let Some(io_err) = err.downcast_ref::<io::Error>() {
                    if io_err.kind() == io::ErrorKind::UnexpectedEof {
                        // abrupt disconnect, gmail seems to do this one after 24h
                        let event = AnvilopeEvent::AccountDisconnect {
                            account_key: local_listener.account_key.to_owned()
                        };
                        local_listener.root_tx.send(event).await.unwrap();
                        // retry the failed message download
                        let event = AnvilopeEvent::AccountNewMessage {
                            account_key: local_listener.account_key.to_owned(),
                            mailbox: mailbox.to_owned(),
                            ord: ord.to_owned(),
                        };
                        local_listener.root_tx.send(event).await.unwrap();
                    }
                }
                error!("download_message error: {:#?}", err);
            }
        }
    }

    async fn download_message_inner(&mut self, mailbox: &String, ord: &u32) -> Result<AnvilopeEvent> {
        let mut session = self.session_cell.replace(None)
            .ok_or_else(|| anyhow!("expected active IMAP session, found None"))?;

        session.select(mailbox).await?;

        let stream = session.fetch(
            ord.to_string(),
            "(UID RFC822.SIZE BODY.PEEK[HEADER] BODY.PEEK[TEXT])",
        ).await?;
        let msgs: Vec<_> = stream.try_collect().await?;
        let msg = msgs.first().expect("no messages!");

        // always put the session back
        self.session_cell.replace(Some(session));

        let header = msg.header().expect("no header!");
        let body = msg.text().expect("no body!");
        let body = std::str::from_utf8(body).expect("body not UTF-8").to_string();

        let mut db_record = parse::parse_header(&self.account_key, &header)?;
        db_record.body = body;
        let db_record = Message::create(db_record).await?;

        info!("downloaded message from {} for account {}", db_record.from_address, self.account_key);
        debug!("wrote message to database id={}", db_record.id);

        Ok(AnvilopeEvent::MessageDownloaded { message_id: db_record.id })
    }
}
