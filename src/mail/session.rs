use anyhow::{
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
use futures::{StreamExt, TryStreamExt};
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
use stop_token::StopSource;
use tokio::sync::mpsc::Sender;

use crate::{anvilope_config, AnvilopeEvent};
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
    pub session_cell: Mutex<Cell<Option<Session<TransportStream>>>>,
    pub idle_stop_src: Mutex<Cell<Option<StopSource>>>,
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
        let session_cell = Mutex::new(Cell::new(Some(session)));
        let idle_stop_src = Mutex::new(Cell::new(None));

        Ok(ImapSessionManager {
            account_key: account._key.to_owned(),
            root_tx,
            session_cell,
            idle_stop_src,
            connected_at,
            healthy_at,
            backoff_count,
        })
    }

    pub async fn logout(&self) -> Result<()> {
        let session_cell = self.session_cell.lock().await;
        session_cell.take().unwrap().logout().await?;
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
            if io_err.kind() == io::ErrorKind::UnexpectedEof || io_err.kind() == io::ErrorKind::BrokenPipe || io_err.kind() == io::ErrorKind::ConnectionRefused || io_err.kind() == io::ErrorKind::ConnectionReset {
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

    pub fn spawn_listener(sess_man: Arc<ImapSessionManager>) -> () {
        tokio::spawn(async move {
            let result = sess_man.listen_for_messages().await;
            match result {
                Ok(event) => {
                    sess_man.root_tx.send(event).await.unwrap();
                }
                Err(err) => {
                    error!("listen_for_messages() error: {:#?}", err);
                    Self::spawn_reconnect_handler(
                        &err,
                        sess_man.account_key.to_owned(),
                        sess_man.healthy_at,
                        sess_man.backoff_count,
                        sess_man.root_tx.clone(),
                    );
                }
            };
        });
    }

    pub fn spawn_interrupt_copy_message(
        sess_man: Arc<ImapSessionManager>,
        src_mailbox: String,
        uid: u32,
        class_key: String,
    ) -> () {
        tokio::spawn(async move {
            let interrupted = sess_man.clone().interrupt_idle().await;
            let result = sess_man.copy_message_inner(src_mailbox, uid, class_key).await;
            match result {
                Ok(_) => {
                    if interrupted {
                        let account_key = sess_man.account_key.to_owned();
                        sess_man.root_tx.send(AnvilopeEvent::AccountIdleTimeout { account_key }).await.unwrap();
                    }
                }
                Err(err) => {
                    error!("copy_message() error: {:#?}", err);
                    Self::spawn_reconnect_handler(
                        &err,
                        sess_man.account_key.to_owned(),
                        sess_man.healthy_at,
                        sess_man.backoff_count,
                        sess_man.root_tx.clone(),
                    );
                }
            }
        });
    }

    async fn copy_message_inner(&self, src_mailbox: String, uid: u32, class_key: String) -> Result<()> {
        let session_cell = self.session_cell.lock().await;
        let mut session = session_cell.take().unwrap();

        let class = anvilope_config!().classes.iter().find(|c| c._key == class_key).unwrap();
        let dest_folder = class.labels.get(&self.account_key).unwrap();
        debug!("class_key = {}, {} -> {}", class_key, src_mailbox, dest_folder);

        // check for destination folder
        let ls_res = session.list(None, Some(dest_folder))
            .await?
            .collect::<Vec<_>>()
            .await;
        let dest_folder_exists = ls_res.iter().find(|maybe| {
            if let Ok(name) = maybe {
                name.name() == dest_folder
            } else {
                false
            }
        }).is_some();

        if dest_folder_exists {
            session.select(src_mailbox).await?;
            session.uid_copy(uid.to_string(), dest_folder).await?;

            debug!("uid_copy to {} finished", uid);
        } else {
            warn!("label {} does not exist for account {}", dest_folder, self.account_key);
        }

        // be polite, always put the session back
        session_cell.set(Some(session));

        Ok(())
    }

    async fn interrupt_idle(&self) -> bool {
        let stop_src = self.idle_stop_src.lock().await.take();
        if let Some(stop_src) = stop_src {
            drop(stop_src);
            debug!("interrupt IDLE");
            return true;
        }
        debug!("interrupt requested, IDLE not running");
        return false;
    }

    async fn listen_for_messages(&self) -> Result<AnvilopeEvent> {
        info!("entering idle state");

        let session_cell = self.session_cell.lock().await;
        let session = session_cell.take().unwrap();
        let mut idle_handle = session.idle();
        idle_handle.init().await?;

        // TODO: use interrupt token to logout from server gracefully
        let (idle_wait, stop_src) = idle_handle.wait_with_timeout(IDLE_DURATION);
        self.idle_stop_src.lock().await.set(Some(stop_src));

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
                Ok(AnvilopeEvent::Noop { account_key })
            }
            IdleResponse::Timeout => {
                let account_key = self.account_key.to_owned();
                Ok(AnvilopeEvent::AccountIdleTimeout { account_key })
            }
        };

        self.idle_stop_src.lock().await.take();

        // be polite, always put the session back
        let session = idle_handle.done().await?;
        session_cell.set(Some(session));

        result
    }

    pub async fn download_message(sess_man: Arc<ImapSessionManager>, mailbox: String, ord: u32) -> () {
        let result = sess_man.download_message_inner(&mailbox, &ord).await;
        match result {
            Ok(event) => {
                sess_man.root_tx.send(event).await.unwrap();
            }
            Err(err) => {
                // TODO: message likely not downloaded, should be retried
                error!("download_message() error: {:#?}", err);
                Self::spawn_reconnect_handler(
                    &err,
                    sess_man.account_key.to_owned(),
                    sess_man.healthy_at,
                    sess_man.backoff_count,
                    sess_man.root_tx.clone(),
                );
            }
        }
    }

    async fn download_message_inner(&self, mailbox: &String, ord: &u32) -> Result<AnvilopeEvent> {
        let session_cell = self.session_cell.lock().await;
        let mut session = session_cell.take().unwrap();

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

        // attempt fetch text/plain
        let mut body_plain = "".to_string();
        if let Some(part) = parts.iter().find(|tp| tp.mime_subtype == TextPartMimeType::Plain) {
            let section_path = part.to_section_path();
            let path_string = part.to_string();
            debug!("found text/plain, part={}, type={:#?}, encoding={:#?}", path_string, part.mime_subtype, part.encoding);
            let res: Vec<_> = session.uid_fetch(uid.to_string(), format!("BODY.PEEK[{}]", path_string))
                .await?
                .try_collect()
                .await?;
            if res.len() > 0 {
                if let Some(bytes) = res.first().unwrap().section(&section_path) {
                    if let Some(decoded) = parse::decode_utf8(bytes, &part.encoding) {
                        body_plain = decoded;
                    } else {
                        warn!("could not decode text/plain part into UTF-8");
                    }
                } else {
                    warn!("could not download expected text/plain part");
                }
            }
        }

        // attempt fetch text/html
        let mut body_html = "".to_string();
        if let Some(part) = parts.iter().find(|tp| tp.mime_subtype == TextPartMimeType::Html) {
            let section_path = part.to_section_path();
            let path_string = part.to_string();
            debug!("found text/html, part={}, type={:#?}, encoding={:#?}", path_string, part.mime_subtype, part.encoding);
            let res: Vec<_> = session.uid_fetch(uid.to_string(), format!("BODY.PEEK[{}]", path_string))
                .await?
                .try_collect()
                .await?;
            if res.len() > 0 {
                if let Some(bytes) = res.first().unwrap().section(&section_path) {
                    if let Some(decoded) = parse::decode_utf8(bytes, &part.encoding) {
                        body_html = decoded;
                    } else {
                        warn!("could not decode text/html part into UTF-8");
                    }
                } else {
                    warn!("could not download expected text/html part");
                }
            }

        }

        if body_plain.len() == 0 && body_html.len() == 0 {
            warn!("no text/plain or text/html parts found in message");
        }

        // TODO: can probably find a way to cache this safely
        let mailbox_status = session.status(mailbox, "(UIDVALIDITY)").await?;
        let uid_validity = if let Some(uv) = mailbox_status.uid_validity {
            uv
        } else {
            error!("server did not return UIDVALIDITY value");
            0
        };

        // always put the session back
        session_cell.set(Some(session));

        let mut db_record = parse::parse_header(&self.account_key, &header)?;
        db_record.uid = uid;
        db_record.uid_validity = uid_validity;
        db_record.body_plain = body_plain;
        db_record.body_html = body_html;
        let db_record = Message::create(db_record).await?;

        info!("downloaded message from {} for account {}", db_record.from_address, self.account_key);
        debug!("wrote message to database id={}", db_record.id);

        Ok(AnvilopeEvent::MessageDownloaded { message_id: db_record.id })
    }
}
