use anyhow::{
    anyhow,
    Error,
    Result,
};
use home;
use log::{
    debug,
    error,
};
use regex::Regex;
use rustls_platform_verifier::ConfigVerifierExt;
use serde::{
    Deserialize,
    Serialize,
};
use std::collections::HashMap;
use std::fs::File;
use std::io::prelude::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::rustls::ClientConfig;
use toml::{
    map::Map,
    Table,
    Value,
};
use url::Url;

#[derive(Debug, PartialEq, Eq)]
pub enum RunMode {
    Production,
    Test,
}

#[derive(Debug)]
pub struct AnvilopeConfig {
    pub config_dir: PathBuf,
    pub db_path: PathBuf,
    pub tls_client_config: Arc<ClientConfig>,
    pub llm: Llm,
    pub accounts: Vec<Account>,
    pub classifier: Classifier,
    pub classes: Vec<Class>,
}

#[derive(Debug, Deserialize)]
pub struct Llm {
    pub ollama_host: String,
    pub ollama_model: String,

    #[serde(default = "Llm::default_context_length")]
    pub context_length: usize,

    #[serde(default = "Llm::default_temperature")]
    pub temperature: f64,
}

#[derive(Debug, Deserialize)]
pub struct Account {
    pub _key: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,

    #[serde(default = "Account::default_use_tls")]
    pub use_tls: bool,
}

#[derive(Debug, Deserialize)]
pub struct Classifier {
    pub _db_id: Option<i64>,
    pub prompt: String,
    pub script: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Class {
    pub _key: String,
    pub _db_id: Option<i64>,
    pub prompt: String,
    pub labels: HashMap<String, String>,
}

impl Llm {
    fn default_context_length() -> usize { 2048 }

    fn default_temperature() -> f64 { 0.7 }
}

impl Account {
    fn default_use_tls() -> bool { true }
}

impl Classifier {
    pub fn set_db_id(&mut self, id: i64) -> Result<()> {
        self._db_id = Some(id);
        Ok(())
    }
}

impl Class {
    pub fn set_db_id(&mut self, id: i64) -> Result<()> {
        self._db_id = Some(id);
        Ok(())
    }
}

impl AnvilopeConfig {
    pub fn new(mode: RunMode) -> AnvilopeConfig {
        let config_dir = match mode {
            RunMode::Production => {
                let home_dir = home::home_dir().unwrap();
                let config_dir = home_dir.join(".anvilope/");
                config_dir
            },
            RunMode::Test => {
                std::fs::canonicalize("./tests/data").unwrap()
            },
        };
        let db_path = config_dir.join("anvilope.sqlite");
        let config_file_path = config_dir.join("anvilope.toml");
        let config_file_path_2 = config_file_path.clone();

        let mut config_file = File::open(config_file_path)
            .expect("cannot open anvilope.toml file");
        let mut config_str = String::new();
        config_file.read_to_string(&mut config_str)
            .expect("cannot read anvilope.toml file");
        let root_config = config_str.parse::<Table>()
            .expect("cannot parse anvilope.toml file");

        // read individual config file sections into structs
        let llm = Self::parse_llm_section(&root_config)
            .expect("cannot parse [llm] section of config");
        let accounts = Self::parse_accounts_section(&root_config)
            .expect("cannot parse [accounts] section of config");
        let mut classifier = Self::parse_classifier_section(&root_config)
            .expect("cannot parse [classifier] section of config");
        let mut classes = Self::parse_classes_section(&root_config, &accounts)
            .expect("cannot parse [classes] section of config");

        // some hygiene
        classifier.prompt = classifier.prompt.trim().to_string();
        for c in classes.iter_mut() {
            c.prompt = c.prompt.trim().to_string();
        }

        // setup TLS
        let tls_client_config = Arc::new(ClientConfig::with_platform_verifier());

        debug!("configuration loaded: {:#?}", config_file_path_2);

        return AnvilopeConfig {
            config_dir: config_dir.clone(),
            db_path: db_path.clone(),
            tls_client_config,
            llm,
            accounts,
            classifier,
            classes,
        };
    }

    pub fn validate(&self) -> Result<()> {
        let mut error = false;

        // check inference endpoint
        if let Err(_) = Url::parse(self.llm.ollama_host.as_str()) {
            error!("ollama_host is not a valid URL");
            error = true;
        }

        if self.accounts.len() < 1 {
            error!("no IMAP accounts configured!");
            error = true;
        }

        // check IMAP host names
        for acct in self.accounts.iter() {
            let test_url = format!("foobar://{}:{}/", acct.host, acct.port);
            if let Err(_) = Url::parse(test_url.as_str()) {
                error!("{}.host is not a valid host name", acct._key);
                error = true;
            }
        }

        // check prompt template
        let re_classes = Regex::new(r"\{\{\s*classes\s*\}\}")?;
        let re_email = Regex::new(r"\{\{\s*email_message\s*\}\}")?;
        if !re_classes.is_match(self.classifier.prompt.as_str()) {
            error!("classifier prompt lacks a \"{{{{ classes }}}}\" variable");
            error = true;
        }
        if !re_email.is_match(self.classifier.prompt.as_str()) {
            error!("classifier prompt lacks a \"{{{{ email_message }}}}\" variable");
            error = true;
        }

        if self.classes.len() < 1 {
            error!("no classes configured!");
            error = true;
        }

        if error {
            return Err(Error::msg("config validation failed"));
        }

        Ok(())
    }

    pub fn ensure_files() -> () {
        // TODO: create dir and files if not exist
        ()
    }

    fn parse_llm_section(root: &Map<String, Value>) -> Result<Llm> {
        let section = root.get("llm")
            .ok_or(anyhow!("required [llm] section not present in config"))?
            .to_owned();
        let llm: Llm = section.try_into()?;

        Ok(llm)
    }

    fn parse_accounts_section(root: &Map<String, Value>) -> Result<Vec<Account>> {
        let section = root.get("accounts")
            .ok_or(anyhow!("required [accounts] section is missing"))?;
        let keys = section.as_table()
            .ok_or(anyhow!("required [accounts] section is malformed"))?
            .keys();
        let mut accounts: Vec<Account> = Vec::new();
        for k in keys {
            let mut account_map = section[k].as_table()
                .ok_or(anyhow!("malformed account entry: {}", k))?
                .to_owned();
            // Retain the user-provided key
            account_map.insert("_key".to_string(), Value::String(k.to_owned()));
            let account = account_map.try_into()?;
            accounts.push(account);
        }

        Ok(accounts)
    }

    fn parse_classifier_section(root: &Map<String, Value>) -> Result<Classifier> {
        let section = root.get("classifier")
            .ok_or(anyhow!("required [classifier] section is missing"))?;
        let classifier = section.as_table()
            .ok_or(anyhow!("malformed [classifier] section"))?
            .to_owned()
            .try_into()?;

        Ok(classifier)
    }

    fn parse_classes_section(root: &Map<String, Value>, accounts: &Vec<Account>) -> Result<Vec<Class>> {
        let section = root.get("classes")
            .ok_or(anyhow!("required [classes] section is missing"))?;
        let keys = section.as_table()
            .ok_or(anyhow!("required [classes] section is malformed"))?
            .keys();

        let mut classes: Vec<Class> = Vec::new();
        for k in keys {
            let class_map = section[k].as_table()
                .ok_or(anyhow!("malformed class entry: {}", k))?
                .to_owned();

            let _key = k.to_owned();
            let prompt = class_map.get("prompt").unwrap().as_str().unwrap().to_string();
            let mut labels: HashMap<String, String> = HashMap::new();

            // labels are optional, default to account key string
            for acct in accounts {
                labels.insert(acct._key.to_owned(), acct._key.to_owned());
            }
            if let Some(src_labels_value) = class_map.get("labels") {
                let src_labels_map = src_labels_value.as_table().unwrap();
                for (k, v) in src_labels_map.iter() {
                    labels.insert(k.to_owned(), v.as_str().unwrap().to_owned());
                }
            }

            classes.push(Class {
                _key,
                _db_id: None,
                prompt,
                labels,
            });
        }

        Ok(classes)
    }
}
