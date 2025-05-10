pub mod config;
pub mod db;
pub mod llm;
pub mod mail;

pub static ANVILOPE_CONFIG: std::sync::OnceLock<config::AnvilopeConfig> = std::sync::OnceLock::new();
pub static DB_POOL: std::sync::OnceLock<r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>> = std::sync::OnceLock::new();

#[macro_export]
macro_rules! anvilope_config {
    () => {
        crate::ANVILOPE_CONFIG.get().unwrap()
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

pub enum AnvilopeEvent {
    AccountDisconnect { account_key: String }, // possibly reconnect
    AccountIdleTimeout { account_key: String }, // restart idle loop
    AccountNewMessage { account_key: String, mailbox: String, ord: u32 }, // send to message consumer
    MessageDownloaded { message_id: i64 }, // send to llm
    MessageClassified { message_id: i64, class_key: String }, // apply label to message
    Error,
}
