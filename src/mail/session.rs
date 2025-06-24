use anyhow::{
    anyhow,
    Error,
    Result,
};
use async_imap::Session;
use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::{
    MailboxDatum,
    Response,
};
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
    warn,
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
use crate::mail::parse::TextPartMimeType;

use super::TransportStream;
use super::parse;

// TODO: make this configurable because certain firewalls might require a lower
//       duration to keep connection alive
const IDLE_DURATION: Duration = Duration::from_secs(20*60);

const BACKOFF_BASE_SECS: f32 = 30.0;
const MAX_BACKOFF: f32 = 4.0;

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
    pub session_cell: Cell<Option<Session<TransportStream>>>,
    pub root_tx: Sender<AnvilopeEvent>,
    pub connected_at: DateTime<Utc>,
    pub healthy_at: DateTime<Utc>,
    pub backoff_count: f32,
}

impl ImapSessionManager {
    pub async fn new(account: &Account,
                     root_tx: Sender<AnvilopeEvent>,
                     backoff_count: f32) -> Result<ImapSessionManager> {
        info!("connecting to {}", account.host);

        let connected_at = Utc::now();
        let healthy_at = connected_at + Duration::from_secs_f32(BACKOFF_BASE_SECS * backoff_count.exp2());

        let mut session = match super::connect(&account).await {
            Ok(session) => session,
            Err(err) => {
                error!("new() error: {:#?}", err);
                Self::spawn_reconnect_handler(
                    &err,
                    account._key.to_owned(),
                    healthy_at,
                    backoff_count,
                    root_tx.clone(),
                );
                return Err(err);
            }
        };

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

        Ok(ImapSessionManager {
            account_key: account._key.to_owned(),
            root_tx,
            session_cell,
            connected_at,
            healthy_at,
            backoff_count,
        })
    }

    pub async fn logout(&self) -> Result<()> {
        let mut session = self.session_cell.replace(None)
            .ok_or_else(|| anyhow!("expected active IMAP session, found None"))?;
        session.logout().await?;
        Ok(())
    }

    fn spawn_reconnect_handler(
        err: &Error,
        account_key: String,
        healthy_at: DateTime<Utc>,
        backoff_count: f32,
        root_tx: Sender<AnvilopeEvent>
    ) -> () {
        let mut do_reconnect = false;
        if let Some(io_err) = err.root_cause().downcast_ref::<io::Error>() {
            if io_err.kind() == io::ErrorKind::UnexpectedEof || io_err.kind() == io::ErrorKind::BrokenPipe || io_err.kind() == io::ErrorKind::ConnectionRefused {
                do_reconnect = true;
            }
        }
        if do_reconnect {
            tokio::spawn(async move {
                let mut new_backoff = backoff_count;
                let mut backoff_secs = BACKOFF_BASE_SECS * backoff_count.exp2();
                if Utc::now() < healthy_at {
                    new_backoff += 1.0;
                    debug!("new backoff value {} for {}", new_backoff, account_key);
                } else {
                    new_backoff = 0.0;
                    backoff_secs = 0.0;
                    debug!("connection was healthy, backoff value reset for {}", account_key);
                }

                if new_backoff <= MAX_BACKOFF {
                    debug!("sleeping for {}", backoff_secs);
                    tokio::time::sleep(Duration::from_secs_f32(backoff_secs)).await;

                    // signal to recreate ImapSessionManager
                    let event = AnvilopeEvent::AccountDisconnect {
                        account_key: account_key.to_owned(),
                        backoff_count: new_backoff,
                    };
                    root_tx.send(event).await.unwrap();
                } else {
                    error!("max unhealthy reconnects exceeded for account {}, will not reconnect again", account_key);
                }
            });
        }
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
                    error!("listen_for_messages() error: {:#?}", err);
                    Self::spawn_reconnect_handler(
                        &err,
                        local_listener.account_key.to_owned(),
                        local_listener.healthy_at,
                        local_listener.backoff_count,
                        local_listener.root_tx.clone(),
                    );
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
                // TODO: message likely not downloaded, should be retried
                error!("download_message() error: {:#?}", err);
                Self::spawn_reconnect_handler(
                    &err,
                    local_listener.account_key.to_owned(),
                    local_listener.healthy_at,
                    local_listener.backoff_count,
                    local_listener.root_tx.clone(),
                );
            }
        }
    }

    // TODO: add uid column to message table

    async fn download_message_inner(&mut self, mailbox: &String, ord: &u32) -> Result<AnvilopeEvent> {
        let mut session = self.session_cell.replace(None)
            .ok_or_else(|| anyhow!("expected active IMAP session, found None"))?;

        session.select(mailbox).await?;

        let res = session.fetch(
            ord.to_string(),
            "(UID BODYSTRUCTURE BODY.PEEK[HEADER])",
        ).await?;
        let res: Vec<_> = res.try_collect().await?;
        let res = res.first().expect("cannot fetch metadata!");
        let uid = res.uid.unwrap();
        let bodystruct = res.bodystructure().expect("cannot read bodystructure!");
        let header = res.header().expect("cannot read header!");
        let parts = parse::find_text_parts(bodystruct);

        // prefer first text/html part, typically at first level of the email
        let mut maybe_part = parts.iter().find(|tp| tp.mime_subtype == TextPartMimeType::Html);
        if maybe_part.is_none() {
            // fallback to text/plain
            maybe_part = parts.iter().find(|tp| tp.mime_subtype == TextPartMimeType::Plain);
        }
        let mut body = "".to_string();
        if let Some(part) = maybe_part {
            let section_path = part.to_section_path();
            let path_string = part.to_string();
            debug!("selecting part={}, type={:#?}, encoding={:#?}", path_string, part.mime_subtype, part.encoding);
            let res: Vec<_> = session.uid_fetch(uid.to_string(), format!("BODY.PEEK[{}]", path_string))
                .await?
                .try_collect()
                .await?;
            if res.len() > 0 {
                let bytes: &[u8] = res.first().unwrap().section(&section_path).expect("expected body not found!");
                body = parse::decode_utf8(bytes, &part.encoding).expect("could not decode UTF-8");
            }
        } else {
            warn!("could not find suitable body content in email");
        }

        // always put the session back
        self.session_cell.replace(Some(session));

        let mut db_record = parse::parse_header(&self.account_key, &header)?;
        db_record.body = body;
        let db_record = Message::create(db_record).await?;

        info!("downloaded message from {} for account {}", db_record.from_address, self.account_key);
        debug!("wrote message to database id={}", db_record.id);

        Ok(AnvilopeEvent::MessageDownloaded { message_id: db_record.id })
    }
}
