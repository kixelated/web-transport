use std::sync::Arc;

use super::{Connect, ConnectError, Settings, SettingsError};
use futures::StreamExt;
use futures::{future::BoxFuture, stream::FuturesUnordered};
use url::Url;

use crate::{ez, Connection};

#[derive(thiserror::Error, Debug, Clone)]
pub enum ServerError {
    #[error("io error: {0}")]
    Io(Arc<std::io::Error>),

    #[error("settings error: {0}")]
    Settings(#[from] SettingsError),

    #[error("connect error: {0}")]
    Connect(#[from] ConnectError),
}

impl From<std::io::Error> for ServerError {
    fn from(err: std::io::Error) -> Self {
        ServerError::Io(Arc::new(err))
    }
}

pub struct Server {
    inner: ez::Server,
    accept: FuturesUnordered<BoxFuture<'static, Result<Request, ServerError>>>,
}

impl Server {
    /// Wrap an [ez::Server], abstracting away the annoying HTTP/3 handshake required for WebTransport.
    ///
    /// The ALPN must be set to `h3`.
    pub fn new(inner: ez::Server) -> Self {
        Self {
            inner,
            accept: Default::default(),
        }
    }

    /// Accept a new WebTransport session Request from a client.
    pub async fn accept(&mut self) -> Option<Request> {
        loop {
            tokio::select! {
                Some(conn) = self.inner.accept() => self.accept.push(Box::pin(Request::accept(conn))),
                Some(res) = self.accept.next() => {
                    match res {
                        Ok(session) => return Some(session),
                        Err(err) => log::warn!("ignoring failed HTTP/3 handshake: {}", err),
                    }
                }
                else => return None,
            }
        }
    }
}

/// A mostly complete WebTransport handshake, just awaiting the server's decision on whether to accept or reject the session based on the URL.
pub struct Request {
    conn: ez::Connection,
    settings: Settings,
    connect: Connect,
}

impl Request {
    /// Accept a new WebTransport session from a client.
    pub async fn accept(conn: ez::Connection) -> Result<Self, ServerError> {
        // Perform the H3 handshake by sending/reciving SETTINGS frames.
        let settings = Settings::connect(&conn).await?;

        // Accept the CONNECT request but don't send a response yet.
        let connect = Connect::accept(&conn).await?;

        // Return the resulting request with a reference to the settings/connect streams.
        Ok(Self {
            conn,
            settings,
            connect,
        })
    }

    /// Returns the URL provided by the client.
    pub fn url(&self) -> &Url {
        self.connect.url()
    }

    /// Accept the session, returning a 200 OK.
    pub async fn ok(mut self) -> Result<Connection, ServerError> {
        self.connect.respond(http::StatusCode::OK).await?;
        Ok(Connection::new(self.conn, self.settings, self.connect))
    }

    /// Reject the session, returing your favorite HTTP status code.
    pub async fn close(mut self, status: http::StatusCode) -> Result<(), ServerError> {
        self.connect.respond(status).await?;
        Ok(())
    }
}
