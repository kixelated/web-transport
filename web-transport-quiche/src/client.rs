use std::sync::Arc;
use url::Url;

use crate::{
    ez::{self, CertificatePath, DefaultMetrics, Metrics},
    h3, Connection, Settings,
};

#[derive(thiserror::Error, Debug, Clone)]
pub enum ClientError {
    #[error("io error: {0}")]
    Io(Arc<std::io::Error>),

    #[error("settings error: {0}")]
    Settings(#[from] h3::SettingsError),

    #[error("connect error: {0}")]
    Connect(#[from] h3::ConnectError),
}

impl From<std::io::Error> for ClientError {
    fn from(err: std::io::Error) -> Self {
        ClientError::Io(Arc::new(err))
    }
}

pub struct ClientBuilder<M: Metrics = DefaultMetrics>(ez::ClientBuilder<M>);

impl Default for ClientBuilder<DefaultMetrics> {
    fn default() -> Self {
        Self(ez::ClientBuilder::default())
    }
}

impl<M: Metrics> ClientBuilder<M> {
    /// Create a new client builder with the given metrics.
    pub fn with_metrics(m: M) -> Self {
        Self(ez::ClientBuilder::with_metrics(m))
    }

    /// Optional: Listen for incoming packets on the given socket.
    ///
    /// Defaults to an ephemeral port.
    pub fn with_socket(self, socket: std::net::UdpSocket) -> Result<Self, ClientError> {
        Ok(Self(self.0.with_socket(socket)?))
    }

    /// Optional: Listen for incoming packets on the given address.
    ///
    /// Defaults to an ephemeral port.
    pub fn with_bind<A: std::net::ToSocketAddrs>(self, addrs: A) -> Result<Self, ClientError> {
        // We use std to avoid async
        let socket = std::net::UdpSocket::bind(addrs)?;
        self.with_socket(socket)
    }

    /// Use the provided [QuicSettings] instead of the defaults.
    ///
    /// WARNING: [QuicSettings::verify_peer] is set to false by default.
    /// This will completely bypass certificate verification and is generally not recommended.
    pub fn with_settings(self, settings: Settings) -> Self {
        Self(self.0.with_settings(settings))
    }

    // TODO add support for in-memory certs
    pub fn with_cert(self, tls: CertificatePath<'_>) -> Result<Self, ClientError> {
        Ok(Self(self.0.with_cert(tls)?))
    }

    /// Connect to the server with the given host and port.
    ///
    /// This takes ownership because [tokio_quiche] doesn't support reusing the same socket for clients.
    pub async fn connect(self, url: Url) -> Result<Connection, ClientError> {
        let port = url.port().unwrap_or(443);
        let host = url.host().unwrap().to_string();

        let conn = self.0.connect(&host, port).await?;

        Connection::connect(conn, url).await
    }
}
