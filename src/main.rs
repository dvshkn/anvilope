use anvilope::*;

use anyhow::{
    Error,
    Result,
};
use clap::ArgMatches;
use clap::Command;
use config::AnvilopeConfig;
use env_logger::Env;
use log::info;
use tokio::signal::ctrl_c;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use db::{
    LlmJob,
    Message,
};
use llm::Llm;
use mail::account_listener::AccountListener;
use mail::message_consumer::MessageConsumer;

fn init_config() {
    crate::ANVILOPE_CONFIG.get_or_init(|| AnvilopeConfig::new());
}

fn init_globals() {
    init_config();
    crate::DB_POOL.get_or_init(|| db::init_pool());
}

#[tokio::main]
async fn main() -> Result<()> {
    // default log level is INFO if not specified
    env_logger::Builder::from_env(Env::default().default_filter_or("debug")).init();

    let serve_cmd = Command::new("serve")
        .about("start Anvilope server and listen for emails");

    let dryrun_cmd = Command::new("dryrun")
        .about("load and check configuration");

    let damprun_cmd = Command::new("damprun")
        .about("dry run, init the database, connect to email servers, connect to ollama, exit");

    let cli = Command::new("anvilope")
        .about("Anvilope: A heavy-handed approach to email")
        .arg_required_else_help(true)
        .subcommand(serve_cmd)
        .subcommand(dryrun_cmd)
        .subcommand(damprun_cmd);

    match cli.get_matches().subcommand() {
        Some(("serve", sub_matches)) => cli_serve(sub_matches).await?,
        Some(("dryrun", sub_matches)) => cli_dryrun(sub_matches).await?,
        Some(("damprun", sub_matches)) => cli_damprun(sub_matches).await?,
        _ => unreachable!("no commands"),
    }

    Ok(())
}

async fn cli_dryrun(_matches: &ArgMatches) -> Result<()> {
    info!("starting dry run...");

    init_config();
    anvilope_config!().validate()?;
    info!("config validation passed");

    info!("dry run finished");

    Ok(())
}

async fn cli_damprun(_matches: &ArgMatches) -> Result<()> {
    info!("starting damp run...");

    cli_dryrun(&_matches.clone()).await?;

    init_globals();
    db::ensure_tables();
    llm::Llm::sync_prompts().await?;

    // test database by writing a timestamp
    db::Meta::set("cli_init_only".to_string(), db::timestamp().to_string()).await.unwrap();
    info!("database connection is functioning");

    // test configured email accounts
    let (tx, _) = mpsc::channel::<AnvilopeEvent>(4096);
    for acct in &anvilope_config!().accounts {
        let mut consumer = MessageConsumer::new(&acct, tx.clone()).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
        consumer.session.logout().await?;
        // TODO: when write-back support implemented check for existence of destination folders/labels here
    }
    info!("can connect to IMAP server(s)");

    // test ollama
    let llm = Llm::new(tx.clone()).await?;
    let llm_prompt = "Classify the following message as either spam or not spam: This is a spam message.".to_string();
    info!("sending Ollama test prompt: {}", llm_prompt);
    let llm_raw = llm.ollama_generate(llm_prompt).await?;
    let llm_json: serde_json::Value = serde_json::from_str(llm_raw.as_str())?;
    let llm_response = llm_json.as_object()
        .ok_or(Error::msg("Ollama response not a JSON object"))?
        .get("response")
        .ok_or(Error::msg("Ollama response lacks expected JSON property"))?
        .as_str()
        .ok_or(Error::msg("Ollama response has unexpected value type"))?;
    info!("Ollama response: {:?}", llm_response);
    info!("can connect to Ollama server");

    info!("damp run finished");

    Ok(())
}

async fn cli_serve(_matches: &ArgMatches) -> Result<()> {
    info!("starting anvilope server...");

    init_globals();
    db::ensure_tables();

    let (tx, mut rx) = mpsc::channel::<AnvilopeEvent>(4096);

    let mut key_listener_map: HashMap<&str, Arc<Mutex<AccountListener>>> = HashMap::new();
    let mut key_consumer_map: HashMap<&str, Mutex<MessageConsumer>> = HashMap::new();
    for acct in &anvilope_config!().accounts {
        let key = acct._key.as_str();
        let listener = Arc::new(Mutex::new(
            AccountListener::new(&acct, tx.clone()).await?
        ));
        let consumer = Mutex::new(
            MessageConsumer::new(&acct, tx.clone()).await?
        );

        key_listener_map.insert(key, listener.clone());
        key_consumer_map.insert(key, consumer);

        AccountListener::spawn(listener);
    }

    let llm = Arc::new(
        Llm::new(tx.clone()).await?
    );

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event.unwrap() {
                    AnvilopeEvent::AccountIdleTimeout { account_key } => {
                        info!("IDLE has timed out! restarting...");
                        let listener = key_listener_map.get(account_key.as_str()).unwrap().clone();
                        AccountListener::spawn(listener);
                    }
                    AnvilopeEvent::AccountNewMessage { account_key, mailbox, ord } => {
                        info!("new message received! {}, {}, {}", account_key, mailbox, ord);

                        // resume idle loop
                        let listener = key_listener_map.get(account_key.as_str()).unwrap().clone();
                        AccountListener::spawn(listener);

                        // TODO: would be nice to give the downloader its own
                        //       queue so that we don't have to await on this
                        //       in the main loop
                        let consumer = key_consumer_map.get(account_key.as_str()).unwrap();
                        consumer.lock().await.download_message(mailbox, ord).await;
                    }
                    AnvilopeEvent::MessageDownloaded { message_id } => {
                        info!("message downloaded event");
                        info!("message written to database, id={}", message_id);
                        LlmJob::enqueue(message_id).await?;
                        Llm::spawn(llm.clone());
                    }
                    AnvilopeEvent::MessageClassified { message_id, class_key } => {
                        info!("message classified event");
                        info!("updating class of message_id={} to \"{}\"", message_id, class_key);
                        Message::set_class(message_id, class_key).await?;
                    }
                    _ => {
                        println!("not yet");
                        break;
                    }
                }
            }
            _ = ctrl_c() => {
                info!("quit signal received");
                break;
            }
        }
    }

    Ok(())
}
