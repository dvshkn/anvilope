pub mod session;
pub mod parse;

use anyhow::Result;
use async_imap::{
    Client,
    Session,
};
use log::info;
use rustls_pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::{
    TlsConnector,
    client::TlsStream,
};

use crate::{
    anvilope_config,
    config::Account,
};

pub type AnvilopeTlsStream = TlsStream<TcpStream>;

pub async fn connect(account: &Account) -> Result<Session<TlsStream<TcpStream>>> {
    let dns_name = ServerName::try_from(account.host.to_owned())?;
    let host_port = format!("{}:{}", account.host, account.port);
    let connector = TlsConnector::from(anvilope_config!().tls_client_config.clone());
    let socket = TcpStream::connect(&host_port).await?;
    let tls_stream = connector.connect(dns_name, socket).await?;
    let client = Client::new(tls_stream);


    let imap_session = match client.login(account.username.to_owned(), account.password.to_owned()).await {
        Ok(s) => s,
        Err(_) => panic!("cannot"),
    };

    info!("connected to IMAP server {}", host_port);

    Ok(imap_session)
}
