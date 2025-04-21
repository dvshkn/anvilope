use anyhow::{
    Error,
    Result,
};
use async_imap::extensions::idle::Handle;
use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::MailboxDatum;
use async_imap::imap_proto::types::Response;
use async_imap::types::Capability;
use log::{
    debug,
    error,
    info,
};
use tokio::sync::Mutex;
use std::time::Duration;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

use crate::AnvilopeEvent;
use crate::config::Account;

use super::AnvilopeTlsStream;

const IDLE_DURATION: Duration = Duration::from_secs(5*60);

#[derive(PartialEq)]
pub enum AccountListenerEvent {
    Quit,
    Data,
    Timeout,
}

pub struct AccountListener {
    pub account_key: String,
    pub idle_handle: Handle<AnvilopeTlsStream>,
    pub root_tx: Sender<AnvilopeEvent>,
}

impl AccountListener {
    pub async fn new(account: &Account,
                     root_tx: Sender<AnvilopeEvent>) -> Result<AccountListener> {
        info!("connecting to {}", account.host);

        let mut session = super::new_imap_session(&account).await?;

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

        info!("starting IMAP idle");
        let mut idle_handle = session.idle();
        idle_handle.init().await?;

        Ok(AccountListener { account_key: account._key.to_owned(), root_tx, idle_handle })
    }

    pub fn spawn(listener_ref: Arc<Mutex<AccountListener>>) -> () {
        tokio::spawn(async move {
            let mut local_listener = listener_ref.lock().await;
            let result = local_listener.listen_for_messages().await;
            match result {
                Ok(event) => {
                    local_listener.root_tx.send(event).await.unwrap();
                }
                Err(err) => {
                    error!("listen_for_messages error: {:#?}", err);
                }
            };
        });
    }

    async fn listen_for_messages(&mut self) -> Result<AnvilopeEvent> {
        info!("entering idle state");
        // TODO: use interrupt token to logout from server gracefully
        let (idle_wait, _interrupt) = self.idle_handle.wait_with_timeout(IDLE_DURATION);

        match idle_wait.await? {
            IdleResponse::NewData(data) => {
                if let Response::MailboxData(MailboxDatum::Exists(ordinal)) = data.parsed() {
                    let ord = *ordinal;
                    info!("new message! ord = {}", ord);
                    return Ok(AnvilopeEvent::AccountNewMessage {
                        account_key: self.account_key.to_owned(),
                        mailbox: "INBOX".to_string(),
                        ord,
                    });
                } else {
                    // other activity happening in the mailbox, resume idle
                    debug!("other new data received, resuming idle...");
                    let account_key = self.account_key.to_owned();
                    return Ok(AnvilopeEvent::AccountIdleTimeout { account_key });
                }
            }
            IdleResponse::ManualInterrupt => {
                panic!("unexpected manual interrupt!");
            }
            IdleResponse::Timeout => {
                let account_key = self.account_key.to_owned();
                return Ok(AnvilopeEvent::AccountIdleTimeout { account_key });
            }
        };
    }
}
