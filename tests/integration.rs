mod utils;

use anyhow::Result;
use lettre::Message;
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use std::sync::Arc;
use tokio::sync::mpsc;

use anvilope::*;
use anvilope::mail::session::ImapSessionManager;

use crate::utils::greenmail;

#[tokio::test]
async fn test_receive_message() -> Result<()> {
    utils::init().await?;

    let (tx, mut rx) = mpsc::channel::<AnvilopeEvent>(4096);
    let account = anvilope_config!().accounts.first().unwrap();
    let session = ImapSessionManager::new(account, tx.clone(), 0.0).await?;
    let session_ref = Arc::new(session);
    ImapSessionManager::spawn_listener(session_ref.clone());

    let message = Message::builder()
        .from(Mailbox::new(Some("Robert Tortoise".to_owned()), "robert.tortoise@zaibatsu.corp".parse().unwrap()))
        .to(Mailbox::new(None, "alice@glitch.city".parse().unwrap()))
        .subject("Greenmail Test")
        .header(ContentType::TEXT_PLAIN)
        .body("Sending this in plain text because I know you hate HTML mail :-)".to_string())
        .unwrap();

    utils::greenmail::send(message).await?;

    let event = rx.recv().await.unwrap();
    match event {
        AnvilopeEvent::AccountNewMessage { account_key, mailbox, ord } => {
            let expected_ord = greenmail::message_count("alice").await?;
            assert!(account_key == "greenmail");
            assert!(mailbox == "INBOX");
            assert!(ord == expected_ord);
        },
        _ => {
            assert!(false);
        },
    };

    session_ref.logout().await?;

    Ok(())
}

#[tokio::test]
async fn test_download_message() -> Result<()> {
    utils::init().await?;

    let (tx, mut rx) = mpsc::channel::<AnvilopeEvent>(4096);
    let account = anvilope_config!().accounts.first().unwrap();
    let session = ImapSessionManager::new(account, tx.clone(), 0.0).await?;
    let session_ref = Arc::new(session);

    let message = Message::builder()
        .from(Mailbox::new(Some("Robert Tortoise".to_owned()), "robert.tortoise@zaibatsu.corp".parse().unwrap()))
        .to(Mailbox::new(None, "alice@glitch.city".parse().unwrap()))
        .subject("Greenmail Test")
        .header(ContentType::TEXT_PLAIN)
        .body("Sending this in plain text because I know you hate HTML mail :-)".to_string())
        .unwrap();

    utils::greenmail::send(message).await?;

    let expected_ord = greenmail::message_count("alice").await?;
    ImapSessionManager::download_message(session_ref.clone(), "INBOX".to_string(), expected_ord).await;
    let event = rx.recv().await.unwrap();
    match event {
        AnvilopeEvent::MessageDownloaded { message_id, .. } => {
            assert!(message_id > 0);
        },
        _ => {
            assert!(false);
        },
    }

    session_ref.logout().await?;

    Ok(())
}
