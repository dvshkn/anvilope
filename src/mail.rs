pub mod session;
pub mod parse;

use anyhow::Result;
use async_imap::{
    Client,
    Session,
};
use core::task::{Context, Poll};
use log::info;
use rustls_pki_types::ServerName;
use std::fmt;
use std::io::IoSlice;
use std::pin::Pin;
use tokio::io::{
    AsyncRead,
    AsyncWrite,
    ReadBuf
};
use tokio::net::TcpStream;
use tokio_rustls::{
    TlsConnector,
    client::TlsStream,
};

use crate::{
    anvilope_config,
    config::Account,
};

pub type TransportStream = MaybeTlsStream<TcpStream>;

pub async fn connect(account: &Account) -> Result<Session<TransportStream>> {
    let dns_name = ServerName::try_from(account.host.to_owned())?;
    let host_port = format!("{}:{}", account.host, account.port);
    let socket = TcpStream::connect(&host_port).await?;
    let stream = if account.use_tls {
        let connector = TlsConnector::from(anvilope_config!().tls_client_config.clone());
        let tls_stream = connector.connect(dns_name, socket).await?;
        MaybeTlsStream::YesTls(tls_stream)
    } else {
        MaybeTlsStream::NoTls(socket)
    };
    let client = Client::new(stream);

    let imap_session = match client.login(account.username.to_owned(), account.password.to_owned()).await {
        Ok(s) => s,
        Err(_) => panic!("cannot"),
    };

    info!("connected to IMAP server {}", host_port);

    Ok(imap_session)
}

// lovingly inspired by hyper-tls
// https://github.com/hyperium/hyper-tls/blob/master/src/stream.rs
pub enum MaybeTlsStream<T> {
    NoTls(T),
    YesTls(TlsStream<T>),
}

impl<T: fmt::Debug> fmt::Debug for MaybeTlsStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MaybeTlsStream::NoTls(s) => f.debug_tuple("No TLS").field(s).finish(),
            MaybeTlsStream::YesTls(s) => f.debug_tuple("Yes TLS").field(s).finish(),
        }
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncRead for MaybeTlsStream<T> {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<std::io::Result<()>> {
        match Pin::get_mut(self) {
            MaybeTlsStream::NoTls(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTlsStream::YesTls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncWrite for MaybeTlsStream<T> {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match Pin::get_mut(self) {
            MaybeTlsStream::NoTls(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTlsStream::YesTls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    #[inline]
    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<std::io::Result<()>> {
        match Pin::get_mut(self) {
            MaybeTlsStream::NoTls(s) => Pin::new(s).poll_flush(cx),
            MaybeTlsStream::YesTls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<std::io::Result<()>> {
        match Pin::get_mut(self) {
            MaybeTlsStream::NoTls(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTlsStream::YesTls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context,
        bufs: &[IoSlice],
    ) -> Poll<std::io::Result<usize>> {
        match Pin::get_mut(self) {
            MaybeTlsStream::NoTls(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            MaybeTlsStream::YesTls(s) => Pin::new(s).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            MaybeTlsStream::NoTls(s) => s.is_write_vectored(),
            MaybeTlsStream::YesTls(s) => s.is_write_vectored(),
        }
    }
}
