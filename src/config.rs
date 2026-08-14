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
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::prelude::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::rustls::ClientConfig;
use toml::{
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

// intermediate representation of the config file, separated from
// AnvilopeConfig so that parsing and validation are unit testable
#[derive(Debug)]
struct ParsedConfig {
    llm: Llm,
    accounts: Vec<Account>,
    classifiers: Vec<Classifier>,
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

    // names of classifiers to run on messages from this account
    #[serde(default)]
    pub classifiers: Vec<String>,

    // actions to apply when a message is classified into a given class
    #[serde(default)]
    pub actions: Vec<Action>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Label,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct Action {
    pub r#type: ActionType,
    pub name: String,
    // "<classifier name>.<class name>"
    pub if_class: String,
}

#[derive(Debug)]
pub struct Classifier {
    pub _key: String,
    pub prompt: String,
    // class name -> class prompt
    pub classes: BTreeMap<String, String>,
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

        let parsed = Self::parse(&root_config)
            .expect("cannot parse anvilope.toml file");

        // some hygiene
        let mut classifiers = parsed.classifiers;
        for c in classifiers.iter_mut() {
            c.prompt = c.prompt.trim().to_string();
            for p in c.classes.values_mut() {
                *p = p.trim().to_string();
            }
        }

        // setup TLS
        let tls_client_config = Arc::new(ClientConfig::with_platform_verifier());

        debug!("configuration loaded: {:#?}", config_file_path_2);

        return AnvilopeConfig {
            config_dir: config_dir.clone(),
            db_path: db_path.clone(),
            tls_client_config,
            llm: parsed.llm,
            accounts: parsed.accounts,
            classifiers,
        };
    }

    pub fn validate(&self) -> Result<()> {
        Self::validate_parsed(&self.llm, &self.accounts, &self.classifiers)
    }

    fn validate_parsed(llm: &Llm, accounts: &Vec<Account>, classifiers: &Vec<Classifier>) -> Result<()> {
        let mut error = false;

        // check inference endpoint
        if let Some(ollama_host) = llm.ollama_host.as_ref() {
            if let Err(_) = Url::parse(ollama_host.as_str()) {
                error!("ollama_host is not a valid URL");
                error = true;
            }
        } else if let Some(llama_cpp_host) = llm.llama_cpp_host.as_ref() {
            if let Err(_) = Url::parse(llama_cpp_host.as_str()) {
                error!("llama_cpp_host is not a valid URL");
                error = true;
            }
        } else {
            error!("no inference API host specified");
            error = true;
        }

        if accounts.len() < 1 {
            error!("no IMAP accounts configured!");
            error = true;
        }

        // check IMAP host names
        for acct in accounts.iter() {
            let test_url = format!("foobar://{}:{}/", acct.host, acct.port);
            if let Err(_) = Url::parse(test_url.as_str()) {
                error!("{}.host is not a valid host name", acct._key);
                error = true;
            }
        }

        if classifiers.len() < 1 {
            error!("no classifiers configured!");
            error = true;
        }

        // check classifier prompts and class names
        let re_classes = Regex::new(r"\{\{\s*classes\s*\}\}")?;
        let re_email = Regex::new(r"\{\{\s*email_message\s*\}\}")?;
        for classifier in classifiers.iter() {
            if !re_classes.is_match(classifier.prompt.as_str()) {
                error!("classifier \"{}\" prompt lacks a \"{{{{ classes }}}}\" variable", classifier._key);
                error = true;
            }
            if !re_email.is_match(classifier.prompt.as_str()) {
                error!("classifier \"{}\" prompt lacks a \"{{{{ email_message }}}}\" variable", classifier._key);
                error = true;
            }
            if classifier.classes.len() < 1 {
                error!("classifier \"{}\" has no classes", classifier._key);
                error = true;
            }
            for class_key in classifier.classes.keys() {
                if class_key.contains('.') {
                    error!("classifier \"{}\" class \"{}\" has an invalid name, dots are reserved", classifier._key, class_key);
                    error = true;
                }
            }
        }

        // check account to classifier wiring and actions
        for acct in accounts.iter() {
            if acct.classifiers.len() < 1 {
                error!("account \"{}\" has no classifiers configured", acct._key);
                error = true;
            }
            for name in acct.classifiers.iter() {
                if !classifiers.iter().any(|c| c._key == *name) {
                    error!("account \"{}\" references undefined classifier \"{}\"", acct._key, name);
                    error = true;
                }
            }
            for action in acct.actions.iter() {
                Self::validate_action(acct, action, classifiers, &mut error);
            }
        }

        if error {
            return Err(Error::msg("config validation failed"));
        }

        Ok(())
    }

    fn validate_action(acct: &Account, action: &Action, classifiers: &Vec<Classifier>, error: &mut bool) {
        let mut parts = action.if_class.splitn(2, '.');
        let cf_key = parts.next().unwrap_or("");
        let class_key = parts.next().unwrap_or("");
        if cf_key.is_empty() || class_key.is_empty() || class_key.contains('.') {
            error!("account \"{}\" action if_class \"{}\" is malformed, expected \"<classifier>.<class>\"", acct._key, action.if_class);
            *error = true;
            return;
        }
        match classifiers.iter().find(|c| c._key == cf_key) {
            None => {
                error!("account \"{}\" action if_class references undefined classifier \"{}\"", acct._key, cf_key);
                *error = true;
            },
            Some(cf) => {
                if !cf.classes.contains_key(class_key) {
                    error!("account \"{}\" action if_class references undefined class \"{}.{}\"", acct._key, cf_key, class_key);
                    *error = true;
                }
                if !acct.classifiers.iter().any(|n| n == cf_key) {
                    error!("account \"{}\" action if_class references classifier \"{}\" which the account does not run", acct._key, cf_key);
                    *error = true;
                }
            },
        }
        if action.name.is_empty() {
            error!("account \"{}\" action for class \"{}\" has an empty name", acct._key, action.if_class);
            *error = true;
        }
    }

    pub fn ensure_files() -> () {
        // TODO: create dir and files if not exist
        ()
    }

    fn parse(root: &Table) -> Result<ParsedConfig> {
        // reject the pre-alpha config format with a helpful message
        if root.contains_key("classifier") || root.contains_key("classes") {
            return Err(anyhow!(
                "config file appears to use the old format (top-level [classifier]/[classes] \
                 sections); please migrate to the new [classifiers.<name>] format"
            ));
        }

        let llm = Self::parse_llm_section(root)?;
        let accounts = Self::parse_accounts_section(root)?;
        let classifiers = Self::parse_classifiers_section(root)?;

        Ok(ParsedConfig { llm, accounts, classifiers })
    }

    fn parse_llm_section(root: &Table) -> Result<Llm> {
        let section = root.get("llm")
            .ok_or(anyhow!("required [llm] section not present in config"))?
            .to_owned();
        let llm: Llm = section.try_into()?;

        Ok(llm)
    }

    fn parse_accounts_section(root: &Table) -> Result<Vec<Account>> {
        let section = root.get("accounts")
            .ok_or(anyhow!("required [accounts] section is missing"))?;
        let section = section.as_table()
            .ok_or(anyhow!("required [accounts] section is malformed"))?;
        let mut accounts: Vec<Account> = Vec::new();
        for (k, v) in section.iter() {
            let mut account_map = v.as_table()
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

    fn parse_classifiers_section(root: &Table) -> Result<Vec<Classifier>> {
        let section = root.get("classifiers")
            .ok_or(anyhow!("required [classifiers] section not present in config"))?;
        let section = section.as_table()
            .ok_or(anyhow!("required [classifiers] section is malformed"))?;

        let mut classifiers: Vec<Classifier> = Vec::new();
        for (k, v) in section.iter() {
            let table = v.as_table()
                .ok_or(anyhow!("malformed classifier entry: {}", k))?;

            let prompt_value = table.get("prompt")
                .ok_or(anyhow!("classifier \"{}\" is missing its prompt", k))?;
            let prompt = prompt_value.as_str()
                .ok_or(anyhow!("classifier \"{}\" prompt is not a string", k))?
                .to_string();

            let classes_value = table.get("classes")
                .ok_or(anyhow!("classifier \"{}\" is missing its classes table", k))?;
            let classes_table = classes_value.as_table()
                .ok_or(anyhow!("classifier \"{}\" classes table is malformed", k))?;
            let mut classes: BTreeMap<String, String> = BTreeMap::new();
            for (class_key, class_value) in classes_table.iter() {
                let class_prompt = class_value.as_str()
                    .ok_or(anyhow!("classifier \"{}\" class \"{}\" prompt is not a string", k, class_key))?;
                classes.insert(class_key.to_owned(), class_prompt.to_owned());
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

#[cfg(test)]
mod tests {
    use super::*;

    const NEW_FORMAT: &str = r#"
[llm]
ollama_host = "http://127.0.0.1:11434"
model = "gemma4:e4b"
context_length = 32768

[accounts]
gmail.host = "imap.gmail.com"
gmail.port = 993
gmail.username = "example_user@gmail.com"
gmail.password = "example_password"
gmail.classifiers = ["categories"]
gmail.actions = [
    { type = "label", name = "my_spam_folder", if_class = "categories.spam" },
    { type = "label", name = "my_personal_emails", if_class = "categories.personal" },
]

[classifiers.categories]
prompt = """
<instructions>
Classify the email message into one of the classes below.
</instructions>

<classes>
{{ classes }}
</classes>

{{ email_message }}
"""
classes.personal = "These are personal emails"
classes.spam = "These are spam emails"
"#;

    const OLD_FORMAT: &str = r#"
[llm]
ollama_host = "http://127.0.0.1:11434"
model = "gemma4:e4b"

[accounts]
gmail.host = "imap.gmail.com"
gmail.port = 993
gmail.username = "example_user@gmail.com"
gmail.password = "example_password"

[classifier]
prompt = """
{{ classes }}
{{ email_message }}
"""

[classes]
personal.prompt = "These are personal emails"
"#;

    fn parse_str(s: &str) -> Result<ParsedConfig> {
        let root: Table = s.parse()?;
        AnvilopeConfig::parse(&root)
    }

    fn valid_parsed() -> ParsedConfig {
        parse_str(NEW_FORMAT).expect("NEW_FORMAT should parse")
    }

    #[test]
    fn test_parse_new_format() {
        let parsed = valid_parsed();

        assert_eq!(parsed.llm.model, "gemma4:e4b");
        assert_eq!(parsed.accounts.len(), 1);
        let acct = &parsed.accounts[0];
        assert_eq!(acct._key, "gmail");
        assert_eq!(acct.classifiers, vec!["categories".to_string()]);
        assert_eq!(acct.actions.len(), 2);
        assert!(matches!(acct.actions[0].r#type, ActionType::Label));
        assert_eq!(acct.actions[0].name, "my_spam_folder");
        assert_eq!(acct.actions[0].if_class, "categories.spam");

        assert_eq!(parsed.classifiers.len(), 1);
        let cf = &parsed.classifiers[0];
        assert_eq!(cf._key, "categories");
        assert_eq!(cf.classes.len(), 2);
        assert_eq!(cf.classes.get("personal").unwrap(), "These are personal emails");
        assert_eq!(cf.classes.get("spam").unwrap(), "These are spam emails");
    }

    #[test]
    fn test_parse_old_format_rejected() {
        let err = parse_str(OLD_FORMAT).unwrap_err();
        assert!(err.to_string().contains("old format"),
            "unexpected error: {}", err);
    }

    #[test]
    fn test_parse_missing_classifiers() {
        let s = "[llm]\nmodel = \"m\"\n[accounts]\na.host = \"h\"\na.port = 1\na.username = \"u\"\na.password = \"p\"\n";
        let err = parse_str(s).unwrap_err();
        assert!(err.to_string().contains("[classifiers]"),
            "unexpected error: {}", err);
    }

    #[test]
    fn test_parse_classifier_missing_classes_table() {
        let s = "[llm]\nmodel = \"m\"\n[accounts]\na.host = \"h\"\na.port = 1\na.username = \"u\"\na.password = \"p\"\n[classifiers.x]\nprompt = \"p\"\n";
        let err = parse_str(s).unwrap_err();
        assert!(err.to_string().contains("classes table"),
            "unexpected error: {}", err);
    }

    #[test]
    fn test_validate_ok() {
        let parsed = valid_parsed();
        AnvilopeConfig::validate_parsed(&parsed.llm, &parsed.accounts, &parsed.classifiers)
            .expect("valid config should pass validation");
    }

    #[test]
    fn test_validate_prompt_lacks_classes_var() {
        let mut parsed = valid_parsed();
        parsed.classifiers[0].prompt = "no variables here {{ email_message }}".to_string();
        let err = AnvilopeConfig::validate_parsed(&parsed.llm, &parsed.accounts, &parsed.classifiers)
            .unwrap_err();
        assert!(err.to_string().contains("config validation failed"),
            "unexpected error: {}", err);
    }

    #[test]
    fn test_validate_prompt_lacks_email_var() {
        let mut parsed = valid_parsed();
        parsed.classifiers[0].prompt = "no variables here {{ classes }}".to_string();
        AnvilopeConfig::validate_parsed(&parsed.llm, &parsed.accounts, &parsed.classifiers)
            .unwrap_err();
    }

    #[test]
    fn test_validate_class_name_with_dot() {
        let mut parsed = valid_parsed();
        parsed.classifiers[0].classes.insert("bad.name".to_string(), "p".to_string());
        AnvilopeConfig::validate_parsed(&parsed.llm, &parsed.accounts, &parsed.classifiers)
            .unwrap_err();
    }

    #[test]
    fn test_validate_action_references_unknown_classifier() {
        let mut parsed = valid_parsed();
        parsed.accounts[0].actions[0].if_class = "nope.spam".to_string();
        AnvilopeConfig::validate_parsed(&parsed.llm, &parsed.accounts, &parsed.classifiers)
            .unwrap_err();
    }

    #[test]
    fn test_validate_action_references_unknown_class() {
        let mut parsed = valid_parsed();
        parsed.accounts[0].actions[0].if_class = "categories.nosuchclass".to_string();
        AnvilopeConfig::validate_parsed(&parsed.llm, &parsed.accounts, &parsed.classifiers)
            .unwrap_err();
    }

    #[test]
    fn test_validate_action_classifier_not_run_by_account() {
        let mut parsed = valid_parsed();
        // add a second classifier and point the action at it, but the account
        // only runs "categories"
        parsed.classifiers.push(Classifier {
            _key: "other".to_string(),
            prompt: "{{ classes }}\n{{ email_message }}".to_string(),
            classes: BTreeMap::from([("spam".to_string(), "p".to_string())]),
        });
        parsed.accounts[0].actions[0].if_class = "other.spam".to_string();
        AnvilopeConfig::validate_parsed(&parsed.llm, &parsed.accounts, &parsed.classifiers)
            .unwrap_err();
    }

    #[test]
    fn test_validate_account_without_classifiers() {
        let mut parsed = valid_parsed();
        parsed.accounts[0].classifiers.clear();
        AnvilopeConfig::validate_parsed(&parsed.llm, &parsed.accounts, &parsed.classifiers)
            .unwrap_err();
    }

    #[test]
    fn test_validate_account_references_unknown_classifier() {
        let mut parsed = valid_parsed();
        parsed.accounts[0].classifiers = vec!["nope".to_string()];
        AnvilopeConfig::validate_parsed(&parsed.llm, &parsed.accounts, &parsed.classifiers)
            .unwrap_err();
    }
}
