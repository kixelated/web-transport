use web_transport_proto::{ConnectRequest, ConnectResponse, VarInt};

use thiserror::Error;
use url::Url;

use crate::ez;

#[derive(Error, Debug, Clone)]
pub enum ConnectError {
    #[error("quic stream was closed early")]
    UnexpectedEnd,

    #[error("protocol error: {0}")]
    Proto(#[from] web_transport_proto::ConnectError),

    #[error("connection error")]
    Connection(#[from] ez::ConnectionError),

    #[error("stream error")]
    Stream(#[from] ez::StreamError),

    #[error("http error status: {0}")]
    Status(http::StatusCode),
}

pub struct Connect {
    // The request that was sent by the client.
    request: ConnectRequest,

    // A reference to the send/recv stream, so we don't close it until dropped.
    send: ez::SendStream,

    #[allow(dead_code)]
    recv: ez::RecvStream,
}

impl Connect {
    pub async fn accept(conn: &ez::Connection) -> Result<Self, ConnectError> {
        // Accept the stream that will be used to send the HTTP CONNECT request.
        // If they try to send any other type of HTTP request, we will error out.
        let (send, mut recv) = conn.accept_bi().await?;

        let request = web_transport_proto::ConnectRequest::read(&mut recv).await?;
        log::debug!("received CONNECT request: {request:?}");

        // The request was successfully decoded, so we can send a response.
        Ok(Self {
            request,
            send,
            recv,
        })
    }

    // Called by the server to send a response to the client.
    pub async fn respond(&mut self, status: http::StatusCode) -> Result<(), ConnectError> {
        let resp = ConnectResponse { status };

        log::debug!("sending CONNECT response: {resp:?}");

        let mut buf = Vec::new();
        resp.encode(&mut buf);

        self.send.write_all(&buf).await?;

        Ok(())
    }

    pub async fn open(conn: &ez::Connection, url: Url) -> Result<Self, ConnectError> {
        // Create a new stream that will be used to send the CONNECT frame.
        let (mut send, mut recv) = conn.open_bi().await?;

        // Create a new CONNECT request that we'll send using HTTP/3
        let request = ConnectRequest { url };

        log::debug!("sending CONNECT request: {request:?}");
        request.write(&mut send).await?;

        let response = web_transport_proto::ConnectResponse::read(&mut recv).await?;
        log::debug!("received CONNECT response: {response:?}");

        // Throw an error if we didn't get a 200 OK.
        if response.status != http::StatusCode::OK {
            return Err(ConnectError::Status(response.status));
        }

        Ok(Self {
            request,
            send,
            recv,
        })
    }

    // The session ID is the stream ID of the CONNECT request.
    pub fn session_id(&self) -> VarInt {
        VarInt::try_from(u64::from(self.send.id())).unwrap()
    }

    // The URL in the CONNECT request.
    pub fn url(&self) -> &Url {
        &self.request.url
    }

    pub(super) fn into_inner(self) -> (ez::SendStream, ez::RecvStream) {
        (self.send, self.recv)
    }
}
