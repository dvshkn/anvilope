mod utils;

use anyhow::Result;
use lettre::Message;
use lettre::message::Mailbox;
use lettre::message::header::ContentType;

use anvilope::*;

#[tokio::test]
async fn init_greenmail() -> Result<()> {
    utils::init().await?;
    Ok(())
}

#[tokio::test]
async fn send_email() -> Result<()> {
    let message = Message::builder()
        .from(Mailbox::new(Some("Robert Tortoise".to_owned()), "bob@home.lab".parse().unwrap()))
        .to(Mailbox::new(None, "alice@glitch.city".parse().unwrap()))
        .subject("Re: Need some Cs for this new RFC")
        .header(ContentType::TEXT_PLAIN)
        .body("Somehow I feel that you won't be satisfied with a LGTM :-)  The underlying design is good, but some sections could be clearer.  There's already a lot of verbal explanation of the folder sync.  I think a diagram would help.\n\nBob".to_string())
        .unwrap();

    utils::greenmail::send(message).await?;

    Ok(())
}
