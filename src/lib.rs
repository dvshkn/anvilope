pub mod config;
pub mod db;
pub mod llm;
pub mod mail;

use serde::{
    Deserialize,
    Deserializer,
    Serialize,
    Serializer,
};
use serde::de::Visitor;

pub static ANVILOPE_CONFIG: std::sync::OnceLock<config::AnvilopeConfig> = std::sync::OnceLock::new();
pub static DB_POOL: std::sync::OnceLock<r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>> = std::sync::OnceLock::new();
pub static TEMPLATE_ENV: std::sync::OnceLock<minijinja::Environment> = std::sync::OnceLock::new();

#[macro_export]
macro_rules! anvilope_config {
    () => {
        crate::ANVILOPE_CONFIG.get().unwrap()
    }
}

#[macro_export]
macro_rules! template_env {
    () => {
        crate::TEMPLATE_ENV.get().unwrap()
    }
}

#[macro_export]
macro_rules! db_pool {
    () => {
        crate::DB_POOL.get().unwrap()
    }
}

#[macro_export]
macro_rules! checkout_db_conn {
    () => {
        crate::DB_POOL.get().unwrap().get().unwrap()
    }
}

#[derive(Debug)]
pub enum AnvilopeEvent {
    AccountDisconnect { account_key: String, backoff_count: f32 }, // possibly reconnect
    AccountIdleTimeout { account_key: String }, // restart idle loop
    AccountNewMessage { account_key: String, mailbox: String, ord: u32 }, // send to message consumer
    MessageDownloaded { message_id: i64 }, // send to llm
    MessageClassified { message_id: i64, classes: Vec<ClassifierClass> }, // apply label to message
    Noop { account_key: String },
    Error,
}

#[derive(Debug)]
pub enum ErrorSeverity<T> {
    Ok(T),
    Recoverable(anyhow::Error),
    Reconnectable(anyhow::Error),
    Fatal(anyhow::Error),
}

#[derive(Debug)]
pub struct ClassifierClass {
    pub classifier: String,
    pub class: String,
}

impl TryFrom<&str> for ClassifierClass {
    type Error = anyhow::Error;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let pattern = regex::Regex::new(r"^(?<classifier>\w+)\.(?<class>\w+)$").unwrap();
        if let Some(caps) = pattern.captures(s) {
            Ok(ClassifierClass { classifier: caps["classifier"].to_owned(), class: caps["class"].to_string() })
        } else {
            Err(anyhow::anyhow!("malformed str cannot be converted to ClassifierClass"))
        }
    }
}

impl Serialize for ClassifierClass {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        format!("{}.{}", self.classifier, self.class).serialize(serializer)
    }
}

struct ClassifierClassVisitor;
impl<'de> Visitor<'de> for ClassifierClassVisitor {
    type Value = ClassifierClass;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "a fully qualified ClassifierClass")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where E: serde::de::Error {
        match ClassifierClass::try_from(value) {
            Ok(cc) => Ok(cc),
            Err(e) => Err(serde::de::Error::custom(e)),
        }
    }
}

impl<'de> Deserialize<'de> for ClassifierClass {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: Deserializer<'de> {
        deserializer.deserialize_str(ClassifierClassVisitor)
    }
}
