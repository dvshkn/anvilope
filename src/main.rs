use anvilope::*;

use anyhow::Result;
use clap::{
    Arg,
    ArgMatches,
    Command
};
use config::{
    AnvilopeConfig,
    RunMode,
};
use env_logger::{
    Env,
    Target,
};
use log::{
    debug,
    info,
    warn,
};
use tokio::signal::ctrl_c;
use tokio::sync::mpsc;
use std::collections::HashMap;
use std::sync::Arc;

use db::{
    LlmJob,
    Message,
};
use llm::Llm;
use mail::session::ImapSessionManager;

fn init_config() {
    crate::ANVILOPE_CONFIG.get_or_init(|| AnvilopeConfig::new(RunMode::Production));
    crate::TEMPLATE_ENV.get_or_init(|| {
        let mut template_env = minijinja::Environment::new();
        template_env.add_template("classifier", anvilope_config!().classifier.prompt.as_str()).unwrap();
        template_env.add_template("classes", llm::prompt::TEMPLATE_CLASSES).unwrap();
        template_env.add_template("email_message", llm::prompt::TEMPLATE_EMAIL_MESSAGE).unwrap();
        template_env.add_template("system_prompt", llm::prompt::TEMPLATE_SYSTEM).unwrap();
        template_env
    });
}

fn init_globals() {
    init_config();
    crate::DB_POOL.get_or_init(|| db::init_pool());
}

#[tokio::main]
async fn main() -> Result<()> {
    // default log level is INFO if not specified
    env_logger::Builder::from_env(Env::default().default_filter_or("anvilope=debug"))
        .target(Target::Stdout)
        .init();

    let serve_cmd = Command::new("serve")
        .about("start Anvilope server and listen for emails");

    let dryrun_cmd = Command::new("dryrun")
        .about("load and check configuration");

    let damprun_cmd = Command::new("damprun")
        .about("dry run, init the database, connect to email servers, connect to inference host, exit")
        .arg(Arg::new("skip-imap").help("do not connect to IMAP hosts"))
        .arg(Arg::new("skip-llm").help("do not connect to LLM inference host"));

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

async fn cli_damprun(matches: &ArgMatches) -> Result<()> {
    info!("starting damp run...");

    cli_dryrun(&matches.clone()).await?;

    let (tx, _) = mpsc::channel::<AnvilopeEvent>(4096);
    init_globals();
    db::ensure_tables()?;
    llm::Llm::sync_prompts().await?;

    // test database by writing a timestamp
    db::Meta::set("cli_init_only".to_string(), db::timestamp().to_string()).await.unwrap();
    info!("database connection is functioning");

    // test configured email accounts
    if !matches.contains_id("skip-imap") {
        for acct in &anvilope_config!().accounts {
            let consumer = ImapSessionManager::new(&acct, tx.clone(), 0.0).await?;
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            consumer.logout().await?;
            // TODO: when write-back support implemented check for existence of destination folders/labels here
        }
        info!("can connect to IMAP server(s)");
    }

    // test inference
    if !matches.contains_id("skip-llm") {
        let llm = Llm::new(tx.clone()).await?;
        let llm_prompt = "Classify the following message as either spam or not spam: This is a spam message.".to_string();
        info!("sending test prompt to inference host: {}", llm_prompt);
        let mut chat = llm::prompt::ClassificationChat::new(llm_prompt)?;
        let llm_response = llm.run_chat(&mut chat).await?;
        info!("inference response: {:?}", llm_response);
        info!("can connect to inference host");
    }

    info!("damp run finished");

    Ok(())
}

async fn cli_serve(_matches: &ArgMatches) -> Result<()> {
    info!("starting anvilope server...");

    init_globals();
    anvilope_config!().validate()?;
    db::ensure_tables()?;

    let (tx, mut rx) = mpsc::channel::<AnvilopeEvent>(4096);

    let mut key_listener_map: HashMap<&str, Arc<ImapSessionManager>> = HashMap::new();
    for acct in &anvilope_config!().accounts {
        let key = acct._key.as_str();
        if let Ok(listener) = ImapSessionManager::new(&acct, tx.clone(), 0.0).await {
            let listener_ref = Arc::new(listener);
            key_listener_map.insert(key, listener_ref.clone());
            ImapSessionManager::spawn_listener(listener_ref);
        }
    }

    let llm = Arc::new(
        Llm::new(tx.clone()).await?
    );

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event.unwrap() {
                    AnvilopeEvent::AccountDisconnect { account_key, backoff_count } => {
                        info!("account {} disconnected, attempting reconnect...", account_key);
                        let old_listener = key_listener_map.get(account_key.as_str()).unwrap();
                        let _ = old_listener.logout().await;
                        debug!("logout of old connection finished");
                        let acct = anvilope_config!().accounts.iter().find(|a| a._key == account_key).unwrap();
                        if let Ok(new_listener) = ImapSessionManager::new(&acct, tx.clone(), backoff_count).await {
                            let new_listener_ref = Arc::new(new_listener);
                            key_listener_map.insert(acct._key.as_str(), new_listener_ref.clone());
                            ImapSessionManager::spawn_listener(new_listener_ref);
                        }
                    }
                    AnvilopeEvent::AccountIdleTimeout { account_key } => {
                        info!("IDLE has timed out! restarting...");
                        let listener = key_listener_map.get(account_key.as_str()).unwrap().clone();
                        ImapSessionManager::spawn_listener(listener);
                    }
                    AnvilopeEvent::AccountNewMessage { account_key, mailbox, ord } => {
                        info!("new message received! {}, {}, {}", account_key, mailbox, ord);
                        let listener = key_listener_map.get(account_key.as_str()).unwrap().clone();
                        ImapSessionManager::download_message(listener.clone(), mailbox, ord).await;
                        ImapSessionManager::spawn_listener(listener);
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
                        let message = Message::set_class(message_id, &class_key).await?.unwrap();
                        let account_key = message.account_key;
                        let listener = key_listener_map.get(account_key.as_str()).unwrap().clone();
                        let mailbox = "INBOX".to_string(); // TODO
                        ImapSessionManager::spawn_interrupt_copy_message(
                            listener,
                            mailbox,
                            message.uid,
                            class_key,
                        );
                    }
                    AnvilopeEvent::Noop { account_key } => {
                        debug!("no-op event {}", account_key);
                    }
                    _ => {
                        println!("not yet");
                        break;
                    }
                }
            }
            _ = ctrl_c() => {
                info!("quit signal received");
                for (_, sess_man) in key_listener_map {
                    match sess_man.logout().await {
                        Err(err) => {
                            warn!("{} logout error: {:#?}", sess_man.account_key, err);
                        },
                        Ok(_) => {
                            debug!("{} logout successful", sess_man.account_key);
                        },
                    };
                }
                break;
            }
        }
    }

    Ok(())
}
