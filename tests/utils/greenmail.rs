use anyhow::Result;
use lettre::{
    Message,
    SmtpTransport,
    Transport,
};
use log::{
    debug,
};
use reqwest;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct CreateUserReq {
    pub email: String,
    pub login: String,
    pub password: String,
}

const GREENMAIL_REST_URL: &str = "http://127.0.0.1:8080";
const GREENMAIL_SMTP_URL: &str = "smtp://127.0.0.1:3025";

pub async fn init() -> Result<()> {
    let client = reqwest::Client::new();

    // reset server state
    client.post(format!("{}/api/service/reset", GREENMAIL_REST_URL))
        .send()
        .await?;

    // create alice
    debug!("creating test user alice");
    let body = CreateUserReq {
        email: "alice@glitch.city".to_string(),
        login: "alice".to_string(),
        password: "alice1234".to_string(),
    };
    client.post(format!("{}/api/user", GREENMAIL_REST_URL))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;

    // create bob
    debug!("creating test user bob");
    let body = CreateUserReq {
        email: "robert.tortoise@zaibatsu.corp".to_string(),
        login: "bob".to_string(),
        password: "bob1234".to_string(),
    };
    client.post(format!("{}/api/user", GREENMAIL_REST_URL))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;

    Ok(())
}

pub async fn send(message: Message) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let mailer = SmtpTransport::from_url(GREENMAIL_SMTP_URL)
            .unwrap()
            .build();
        mailer.send(&message)?;
        Ok(())
    }).await?
}

pub async fn message_count(email_or_id: &str) -> Result<u32> {
    let client = reqwest::Client::new();
    let res: Vec<serde_json::Value> = client.get(format!("{}/api/user/{}/messages/{}", GREENMAIL_REST_URL, email_or_id, "INBOX"))
        .send()
        .await?
        .json()
        .await?;
    Ok(res.len() as u32)
}
