use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_quiche::{quiche, quic::HandshakeInfo, ApplicationOverQuic};
use url::Url;

use crate::{ConnectionState, RecvStream, SendStream};

/// WebTransport driver that implements ApplicationOverQuic.
/// Handles HTTP/3 handshake, stream acceptance, and waker notification.
pub struct WebTransportDriver {
    /// Shared connection state with wakers.
    state: Arc<Mutex<ConnectionState>>,

    /// Channel to send new bidirectional streams to the Session.
    bi_tx: mpsc::UnboundedSender<(SendStream, RecvStream)>,

    /// Channel to send new unidirectional streams to the Session.
    uni_tx: mpsc::UnboundedSender<RecvStream>,

    /// Whether the HTTP/3 handshake has completed.
    handshake_complete: bool,

    /// The URL from the CONNECT request (for server side).
    url: Option<Url>,

    /// Whether this is a client or server.
    is_client: bool,
}

impl WebTransportDriver {
    /// Create a new client driver.
    pub fn new_client(
        state: Arc<Mutex<ConnectionState>>,
        bi_tx: mpsc::UnboundedSender<(SendStream, RecvStream)>,
        uni_tx: mpsc::UnboundedSender<RecvStream>,
    ) -> Self {
        Self {
            state,
            bi_tx,
            uni_tx,
            handshake_complete: false,
            url: None,
            is_client: true,
        }
    }

    /// Create a new server driver.
    pub fn new_server(
        state: Arc<Mutex<ConnectionState>>,
        bi_tx: mpsc::UnboundedSender<(SendStream, RecvStream)>,
        uni_tx: mpsc::UnboundedSender<RecvStream>,
    ) -> Self {
        Self {
            state,
            bi_tx,
            uni_tx,
            handshake_complete: false,
            url: None,
            is_client: false,
        }
    }

    /// Process readable streams and wake recv wakers.
    fn process_readable_streams(&mut self, conn: &mut quiche::Connection) {
        // Get all readable streams from Quiche
        while let Some(stream_id) = conn.stream_readable_next() {
            // Wake the waker for this stream if it exists
            let mut state = self.state.lock().unwrap();
            state.wake_recv(stream_id);
        }
    }

    /// Process writable streams and wake send wakers.
    fn process_writable_streams(&mut self, conn: &mut quiche::Connection) {
        // Get all writable streams from Quiche
        while let Some(stream_id) = conn.stream_writable_next() {
            // Wake the waker for this stream if it exists
            let mut state = self.state.lock().unwrap();
            state.wake_send(stream_id);
        }
    }
}

impl ApplicationOverQuic for WebTransportDriver {
    fn on_conn_established(
        &mut self,
        _conn: &mut quiche::Connection,
        _handshake: &HandshakeInfo,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // TODO: Perform HTTP/3 Settings exchange
        // TODO: Handle CONNECT request/response
        // For now, just mark handshake as complete
        self.handshake_complete = true;
        Ok(())
    }

    fn should_act(&self) -> bool {
        // Only process reads/writes after handshake is complete
        self.handshake_complete
    }

    fn buffer(&mut self) -> &mut [u8] {
        // TODO: Return a buffer for outbound packets
        // For now, return an empty slice
        &mut []
    }

    async fn wait_for_data(
        &mut self,
        _conn: &mut quiche::Connection,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // This future completes when the application wants to trigger the worker loop
        // For now, we'll just wait forever since we rely on incoming packets
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
        Ok(())
    }

    fn process_reads(
        &mut self,
        conn: &mut quiche::Connection,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.process_readable_streams(conn);

        // TODO: Accept new streams and decode headers
        // TODO: Send accepted streams to Session via channels

        Ok(())
    }

    fn process_writes(
        &mut self,
        conn: &mut quiche::Connection,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.process_writable_streams(conn);
        Ok(())
    }

    fn on_conn_close<M>(
        &mut self,
        _conn: &mut quiche::Connection,
        _metrics: &M,
        _conn_result: &Result<(), Box<dyn std::error::Error + Send + Sync>>,
    ) {
        // Connection is closing, wake all pending operations
        let mut state = self.state.lock().unwrap();
        state.wake_all_send();
        state.wake_all_recv();
    }
}
