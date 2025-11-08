use super::{Connect, ConnectError, Settings, SettingsError};
use futures::StreamExt;
use futures::{future::BoxFuture, stream::FuturesUnordered};
use url::Url;

use crate::{ez, Session};

#[derive(thiserror::Error, Debug, Clone)]
pub enum ServerError {
    #[error("quiche error: {0}")]
    Quiche(#[from] ez::ServerError),

    #[error("settings error: {0}")]
    Settings(#[from] SettingsError),

    #[error("connect error: {0}")]
    Connect(#[from] ConnectError),
}

pub struct Server {
    inner: ez::Server,
    accept: FuturesUnordered<BoxFuture<'static, Result<Request, ServerError>>>,
}

impl Server {
    /// Manaully create a new server with a manually constructed Endpoint.
    ///
    /// NOTE: The ALPN must be set to `h3` for WebTransport to work.
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
                res = self.inner.accept() => {
                    let conn = res?;
                    self.accept.push(Box::pin(Request::accept(conn)));
                }
                Some(res) = self.accept.next() => {
                    if let Ok(session) = res {
                        return Some(session)
                    }
                }
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
    pub async fn ok(mut self) -> Result<Session, ez::SendError> {
        self.connect.respond(http::StatusCode::OK).await?;
        Ok(Session::new(self.conn, self.settings, self.connect))
    }

    /// Reject the session, returing your favorite HTTP status code.
    pub async fn close(mut self, status: http::StatusCode) -> Result<(), ez::SendError> {
        self.connect.respond(status).await?;
        Ok(())
    }
}
