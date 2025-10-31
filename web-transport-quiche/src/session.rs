use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use url::Url;

use crate::{ConnectionState, RecvStream, SendStream, SessionError};

/// An established WebTransport session over Quiche.
///
/// Similar to Quinn's Connection, but with WebTransport semantics:
/// 1. Streams have headers with session ID
/// 2. Datagrams are prefixed with session ID
/// 3. Error codes are mapped to WebTransport error space
#[derive(Clone)]
pub struct Session {
    /// Shared connection state with wakers.
    state: Arc<Mutex<ConnectionState>>,

    /// Sender for bidirectional stream channel (for cloning).
    bi_tx: mpsc::UnboundedSender<(SendStream, RecvStream)>,

    /// Receiver for bidirectional streams (wrapped to allow cloning).
    bi_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<(SendStream, RecvStream)>>>>,

    /// Sender for unidirectional stream channel (for cloning).
    uni_tx: mpsc::UnboundedSender<RecvStream>,

    /// Receiver for unidirectional streams (wrapped to allow cloning).
    uni_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<RecvStream>>>>,

    /// The URL used to create the session.
    url: Url,
}

impl Session {
    /// Create a new session (internal use only).
    pub(crate) fn new(
        state: Arc<Mutex<ConnectionState>>,
        bi_rx: mpsc::UnboundedReceiver<(SendStream, RecvStream)>,
        uni_rx: mpsc::UnboundedReceiver<RecvStream>,
        url: Url,
    ) -> Self {
        let (bi_tx, _) = mpsc::unbounded_channel();
        let (uni_tx, _) = mpsc::unbounded_channel();

        Self {
            state,
            bi_tx,
            bi_rx: Arc::new(Mutex::new(Some(bi_rx))),
            uni_tx,
            uni_rx: Arc::new(Mutex::new(Some(uni_rx))),
            url,
        }
    }

    /// Get the URL used to create this session.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Accept a new unidirectional stream.
    pub async fn accept_uni(&self) -> Result<RecvStream, SessionError> {
        // Take the receiver out of the Option temporarily
        let mut rx = {
            let mut guard = self.uni_rx.lock().unwrap();
            guard.take().ok_or(SessionError::Closed)?
        };

        // Await without holding the lock
        let result = rx.recv().await;

        // Put the receiver back
        *self.uni_rx.lock().unwrap() = Some(rx);

        result.ok_or(SessionError::Closed)
    }

    /// Accept a new bidirectional stream.
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        // Take the receiver out of the Option temporarily
        let mut rx = {
            let mut guard = self.bi_rx.lock().unwrap();
            guard.take().ok_or(SessionError::Closed)?
        };

        // Await without holding the lock
        let result = rx.recv().await;

        // Put the receiver back
        *self.bi_rx.lock().unwrap() = Some(rx);

        result.ok_or(SessionError::Closed)
    }

    /// Open a new unidirectional stream.
    pub async fn open_uni(&self) -> Result<SendStream, SessionError> {
        // TODO: Properly open a stream via Quiche
        // For now, we'll use a placeholder stream ID
        // This needs to be integrated with the driver to actually open streams
        let state = self.state.clone();
        let stream_id = 0u64; // Placeholder

        // The stream header will be prepended automatically on first write by SendStream
        Ok(SendStream::new(state, stream_id, false))
    }

    /// Open a new bidirectional stream.
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        // TODO: Properly open a stream via Quiche
        // For now, we'll use a placeholder stream ID
        // This needs to be integrated with the driver to actually open streams
        let state = self.state.clone();
        let stream_id = 0u64; // Placeholder

        // The stream header will be prepended automatically on first write by SendStream
        let send = SendStream::new(state.clone(), stream_id, true);
        let recv = RecvStream::new(state, stream_id);

        Ok((send, recv))
    }

    /// Receive an application datagram.
    ///
    /// Waits for a datagram to become available and returns the received bytes.
    /// The session ID header is automatically stripped.
    pub async fn read_datagram(&self) -> Result<Vec<u8>, SessionError> {
        // TODO: Implement datagram reception
        // Need to integrate with the driver to receive datagrams
        Err(SessionError::Closed)
    }

    /// Send an application datagram.
    ///
    /// Datagrams are unreliable and may be dropped or delivered out of order.
    pub fn send_datagram(&self, data: &[u8]) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap();

        // Prepend the session ID header
        let mut buf = Vec::with_capacity(state.header_datagram.len() + data.len());
        buf.extend_from_slice(&state.header_datagram);
        buf.extend_from_slice(data);

        state
            .conn
            .dgram_send(&buf)
            .map_err(|e| SessionError::Quiche(e.into()))?;

        Ok(())
    }

    /// Close the session with an error code and reason.
    pub fn close(&self, error_code: u32, reason: &[u8]) {
        let mut state = self.state.lock().unwrap();
        let code = web_transport_proto::error_to_http3(error_code);
        let _ = state.conn.close(false, code, reason);
    }

    /// Get the maximum datagram size that can be sent.
    pub fn max_datagram_size(&self) -> usize {
        let state = self.state.lock().unwrap();
        state
            .conn
            .dgram_max_writable_len()
            .unwrap_or(0)
            .saturating_sub(state.header_datagram.len())
    }
}

impl web_transport_trait::Session for Session {
    type Error = SessionError;
    type SendStream = SendStream;
    type RecvStream = RecvStream;

    async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        Session::accept_bi(self).await
    }

    async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
        Session::accept_uni(self).await
    }

    async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        Session::open_bi(self).await
    }

    async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
        Session::open_uni(self).await
    }

    fn close(&self, error_code: u32, reason: &str) {
        Session::close(self, error_code, reason.as_bytes());
    }

    fn send_datagram(&self, data: bytes::Bytes) -> Result<(), Self::Error> {
        Session::send_datagram(self, &data)
    }

    async fn recv_datagram(&self) -> Result<bytes::Bytes, Self::Error> {
        let data = Session::read_datagram(self).await?;
        Ok(bytes::Bytes::from(data))
    }

    fn max_datagram_size(&self) -> usize {
        Session::max_datagram_size(self)
    }

    async fn closed(&self) -> Result<(), Self::Error> {
        // TODO: Implement closed detection
        // For now, just wait forever
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
        Ok(())
    }
}
