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
    pub classifiers: HashMap<String, Classifier>,

    // HACK: for backwards compat, remove this
    pub classifier: Classifier,
    pub classes: Vec<Class>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Class {
    pub _key: String,
    pub _db_id: Option<i64>,
    pub prompt: String,
    pub labels: HashMap<String, String>,
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

    #[serde(default)]
    pub actions: Vec<Action>,

    #[serde(default)]
    pub classifiers: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Copy { folder: String, condition: String },
}

#[derive(Debug, Deserialize, Clone)]
pub struct Classifier {
    #[serde(default)]
    pub _db_id: i64,

    #[serde(default)]
    pub _key: String,

    pub prompt: String,
    pub classes: HashMap<String, String>,
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
        let classifiers = Self::parse_classifiers_section(&root_config)
            .expect("cannot parse [classifier] section of config");

        // HACK
        let classes: Vec<Class> = Vec::new();
        let classes2: HashMap<String, String> = HashMap::new();
        let classifier = Classifier { _db_id: 0, _key: "void".to_string(), prompt: "void".to_string(), classes: classes2 };

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

            classifier,
            classes,
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

        // check prompt templates
        let re_classes = Regex::new(r"\{\{\s*classes\s*\}\}")?;
        let re_email = Regex::new(r"\{\{\s*email_message\s*\}\}")?;
        let re_disallowed_chars = Regex::new(r"\W")?;
        self.classifiers.iter().for_each(|(s, c)| {
            if re_disallowed_chars.is_match(s) {
                error!("classifier name '{}' contains disallowed special characters", s);
                error = true;
            }
            if !re_classes.is_match(c.prompt.as_str()) {
                error!("classifier '{}' prompt lacks a \"{{{{ classes }}}}\" variable", s);
                error = true;
            }
            if !re_email.is_match(c.prompt.as_str()) {
                error!("classifier '{}' prompt lacks a \"{{{{ email_message }}}}\" variable", s);
                error = true;
            }
            if c.classes.len() < 1 {
                error!("classifier '{}' has no classes configured!", s);
                error = true;
            }
            c.classes.keys().for_each(|k| {
                if re_disallowed_chars.is_match(k) {
                    error!("class name '{}' contains disallowed special characters", k);
                    error = true;
                }
            });
        });

        if error {
            return Err(Error::msg("config validation failed"));
        }

        Ok(())
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

    fn parse_classifiers_section(root: &Map<String, Value>) -> Result<HashMap<String, Classifier>> {
        let section = root.get("classifiers")
            .ok_or(anyhow!("required [classifiers] section is missing"))?;

        let mut result: HashMap<String, Classifier> = HashMap::new();
        section.as_table()
            .ok_or(anyhow!("malformed [classifiers] section"))?
            .iter()
            .try_for_each(|(s, v)| {
                if let Ok(classifier) = v.to_owned().try_into() {
                    result.insert(s.to_owned(), classifier);
                    Ok(())
                } else {
                    Err(anyhow!("malformed classifier definition '{}', ensure that both prompt and classes are specified", s))
                }
            })?;

        Ok(result)
    }
}
