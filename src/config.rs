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
use std::collections::{
    HashSet,
};
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
    pub classifiers: Vec<Classifier>,
}

#[derive(Debug, Deserialize)]
pub struct Llm {
    pub ollama_host: Option<String>,
    pub llama_cpp_host: Option<String>,
    pub model: String,

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

    /// Classifiers to run on incoming messages for this account.
    /// A message is sent to every classifier listed here. Required: the
    /// [accounts] validation step will reject an account that does not
    /// declare at least one classifier.
    #[serde(default)]
    pub classifiers: Vec<String>,

    /// Actions to perform once a message has been classified. Each action
    /// triggers when the (namespaced) class of a message matches its
    /// `if_class` value.
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// A configurable action executed on classified messages.
///
/// New action types can be added as additional variants of this enum.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Copy the message into the IMAP folder with the given name.
    Label {
        name: String,
        if_class: String,
    },
}

impl Action {
    /// Returns the namespaced class key (eg. "categories.spam") that
    /// triggers this action.
    pub fn if_class(&self) -> &str {
        match self {
            Action::Label { if_class, .. } => if_class,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Classifier {
    pub _key: String,
    pub prompt: String,
    pub classes: Vec<Class>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Class {
    pub _key: String,
    pub prompt: String,
}

impl Llm {
    fn default_context_length() -> usize { 2048 }

    fn default_temperature() -> f64 { 0.7 }
}

impl Account {
    fn default_use_tls() -> bool { true }

    fn username_from_env(_key: &String) -> String {
        let var_name = format!("ANV_CONFIG__ACCOUNTS__{}__USERNAME", _key.to_uppercase());
        if let Ok(value) = std::env::var(var_name) {
            return value.to_owned();
        }
        return "".to_string();
    }

    fn password_from_env(_key: &str) -> String {
        let var_name = format!("ANV_CONFIG__ACCOUNTS__{}__PASSWORD", _key.to_uppercase());
        if let Ok(value) = std::env::var(var_name) {
            return value.to_owned();
        }
        return "".to_string();
    }
}

impl Classifier {
    /// Returns the namespaced key of one of this classifier's classes
    /// (eg. "categories.spam" for classifier "categories" and class "spam").
    pub fn class_key(&self, class_key: &str) -> String {
        format!("{}.{}", self._key, class_key)
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
        let mut classifiers = Self::parse_classifiers_section(&root_config)
            .expect("cannot parse [classifiers] section of config");

        // some hygiene
        for classifier in classifiers.iter_mut() {
            classifier.prompt = classifier.prompt.trim().to_string();
            for class in classifier.classes.iter_mut() {
                class.prompt = class.prompt.trim().to_string();
            }
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
            classifiers,
        };
    }

    pub fn validate(&self) -> Result<()> {
        let mut error = false;

        // check inference endpoint
        if let Some(ollama_host) = self.llm.ollama_host.as_ref() {
            if let Err(_) = Url::parse(ollama_host.as_str()) {
                error!("ollama_host is not a valid URL");
                error = true;
            }
        } else if let Some(llama_cpp_host) = self.llm.llama_cpp_host.as_ref() {
            if let Err(_) = Url::parse(llama_cpp_host.as_str()) {
                error!("llama_cpp_host is not a valid URL");
                error = true;
            }
        } else {
            error!("no inference API host specified");
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

        // check classifier prompt templates
        let re_classes = Regex::new(r"\{\{\s*classes\s*\}\}")?;
        let re_email = Regex::new(r"\{\{\s*email_message\s*\}\}")?;
        if self.classifiers.len() < 1 {
            error!("no classifiers configured!");
            error = true;
        }
        for classifier in self.classifiers.iter() {
            if !re_classes.is_match(classifier.prompt.as_str()) {
                error!("classifier \"{}\" prompt lacks a \"{{{{ classes }}}}\" variable", classifier._key);
                error = true;
            }
            if !re_email.is_match(classifier.prompt.as_str()) {
                error!("classifier \"{}\" prompt lacks a \"{{{{ email_message }}}}\" variable", classifier._key);
                error = true;
            }
            if classifier.classes.len() < 1 {
                error!("classifier \"{}\" has no classes configured!", classifier._key);
                error = true;
            }
        }

        // build set of valid namespaced class keys
        let mut valid_class_keys: HashSet<String> = HashSet::new();
        for classifier in self.classifiers.iter() {
            for class in classifier.classes.iter() {
                valid_class_keys.insert(classifier.class_key(&class._key));
            }
        }

        // check accounts reference valid classifiers and actions
        for acct in self.accounts.iter() {
            if acct.classifiers.is_empty() {
                error!("account \"{}\" does not declare any classifiers (classifiers = [...])", acct._key);
                error = true;
            }
            for classifier_key in acct.classifiers.iter() {
                if !self.classifiers.iter().any(|c| &c._key == classifier_key) {
                    error!("account \"{}\" references unknown classifier \"{}\"", acct._key, classifier_key);
                    error = true;
                }
            }
            for action in acct.actions.iter() {
                let if_class = action.if_class();
                if !valid_class_keys.contains(if_class) {
                    error!("account \"{}\" action references unknown class \"{}\"", acct._key, if_class);
                    error = true;
                }
                match action {
                    Action::Label { name, .. } => {
                        if name.is_empty() {
                            error!("account \"{}\" label action has an empty folder name", acct._key);
                            error = true;
                        }
                    }
                }
            }
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
            if account_map.get("username").is_none() {
                account_map.insert("username".to_string(), Value::String(Account::username_from_env(k)));
            }
            if account_map.get("password").is_none() {
                account_map.insert("password".to_string(), Value::String(Account::password_from_env(k)));
            }

            let account = account_map.try_into()?;
            accounts.push(account);
        }

        Ok(accounts)
    }

    fn parse_classifiers_section(root: &Map<String, Value>) -> Result<Vec<Classifier>> {
        let section = root.get("classifiers")
            .ok_or(anyhow!("required [classifiers] section is missing"))?;
        let table = section.as_table()
            .ok_or(anyhow!("malformed [classifiers] section"))?;

        let mut classifiers: Vec<Classifier> = Vec::new();
        for k in table.keys() {
            let classifier_map = table[k].as_table()
                .ok_or(anyhow!("malformed classifier entry: {}", k))?;

            let prompt = classifier_map.get("prompt")
                .ok_or(anyhow!("classifier \"{}\" is missing a prompt", k))?
                .as_str()
                .ok_or(anyhow!("classifier \"{}\" prompt is not a string", k))?
                .to_string();

            let mut classes: Vec<Class> = Vec::new();
            if let Some(classes_value) = classifier_map.get("classes") {
                let classes_table = classes_value.as_table()
                    .ok_or(anyhow!("malformed classes for classifier \"{}\"", k))?;
                for (class_key, class_value) in classes_table.iter() {
                    let class_prompt = class_value.as_str()
                        .ok_or(anyhow!("class \"{}\" in classifier \"{}\" is not a string", class_key, k))?
                        .to_string();
                    classes.push(Class {
                        _key: class_key.to_owned(),
                        prompt: class_prompt,
                    });
                }
            }

            classifiers.push(Classifier {
                _key: k.to_owned(),
                prompt,
                classes,
            });
        }

        Ok(classifiers)
    }
}
