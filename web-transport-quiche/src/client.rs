use std::net::SocketAddr;

use tokio::sync::mpsc;
use url::Url;

use crate::{ClientError, ConnectionState, Session, WebTransportDriver};

/// A client for connecting to a WebTransport server using Quiche.
#[derive(Clone, Debug)]
pub struct Client {
    // TODO: Store any client configuration here
}

impl Client {
    /// Create a new client with default configuration.
    pub fn new() -> Self {
        Self {}
    }

    /// Connect to a WebTransport server at the given URL.
    ///
    /// This will:
    /// 1. Establish a QUIC connection
    /// 2. Perform HTTP/3 handshake (Settings exchange)
    /// 3. Send CONNECT request
    /// 4. Return a Session on success
    pub async fn connect(&self, url: Url) -> Result<Session, ClientError> {
        // TODO: Parse URL to get host and port
        // TODO: Resolve DNS
        // TODO: Create Quiche config
        // TODO: Call tokio_quiche::connect() with our WebTransportDriver
        // TODO: Wait for handshake to complete
        // TODO: Return Session

        // For now, return a placeholder error
        Err(ClientError::UnexpectedEnd)
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for constructing a WebTransport client with custom configuration.
pub struct ClientBuilder {
    // TODO: Add configuration options
    // - Certificate validation
    // - Congestion control
    // - QUIC parameters
}

impl ClientBuilder {
    /// Create a new client builder with default settings.
    pub fn new() -> Self {
        Self {}
    }

    /// Build the client with the configured settings.
    pub fn build(self) -> Result<Client, ClientError> {
        Ok(Client::new())
    }

    /// Accept the system's root certificates for server validation.
    pub fn with_system_roots(self) -> Result<Client, ClientError> {
        // TODO: Configure certificate validation
        Ok(Client::new())
    }

    /// Accept specific server certificates (for self-signed certs).
    pub fn with_server_certificates(self, _certs: Vec<Vec<u8>>) -> Result<Client, ClientError> {
        // TODO: Configure certificate fingerprints
        Ok(Client::new())
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}
