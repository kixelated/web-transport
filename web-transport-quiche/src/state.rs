use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    task::Waker,
};

use tokio_quiche::quiche;
use web_transport_proto::VarInt;

/// Shared state for a WebTransport session over Quiche.
/// All stream I/O operations grab the lock and call Quiche methods directly.
pub(crate) struct ConnectionState {
    /// The Quiche connection handle.
    pub conn: quiche::Connection,

    /// The WebTransport session ID (derived from CONNECT stream ID).
    pub session_id: VarInt,

    /// Wakers for send streams waiting to write.
    /// Key is the stream ID.
    pub send_wakers: HashMap<u64, Waker>,

    /// Wakers for receive streams waiting to read.
    /// Key is the stream ID.
    pub recv_wakers: HashMap<u64, Waker>,

    /// Pre-computed header for unidirectional streams.
    /// Contains: StreamType::WebTransport + session_id
    pub header_uni: Vec<u8>,

    /// Pre-computed header for bidirectional streams.
    /// Contains: Frame::WebTransport + session_id
    pub header_bi: Vec<u8>,

    /// Pre-computed header for datagrams.
    /// Contains: session_id
    pub header_datagram: Vec<u8>,

    /// Tracks whether the first write has occurred for each send stream.
    /// Used to know when to prepend the header.
    pub stream_first_write: HashMap<u64, bool>,
}

impl ConnectionState {
    /// Creates a new connection state.
    pub fn new(conn: quiche::Connection, session_id: VarInt) -> Arc<Mutex<Self>> {
        let mut header_uni = Vec::new();
        web_transport_proto::StreamUni::WEBTRANSPORT.encode(&mut header_uni);
        session_id.encode(&mut header_uni);

        let mut header_bi = Vec::new();
        web_transport_proto::Frame::WEBTRANSPORT.encode(&mut header_bi);
        session_id.encode(&mut header_bi);

        let mut header_datagram = Vec::new();
        session_id.encode(&mut header_datagram);

        Arc::new(Mutex::new(Self {
            conn,
            session_id,
            send_wakers: HashMap::new(),
            recv_wakers: HashMap::new(),
            header_uni,
            header_bi,
            header_datagram,
            stream_first_write: HashMap::new(),
        }))
    }

    /// Wake a send stream waker if it exists.
    pub fn wake_send(&mut self, stream_id: u64) {
        if let Some(waker) = self.send_wakers.remove(&stream_id) {
            waker.wake();
        }
    }

    /// Wake a receive stream waker if it exists.
    pub fn wake_recv(&mut self, stream_id: u64) {
        if let Some(waker) = self.recv_wakers.remove(&stream_id) {
            waker.wake();
        }
    }

    /// Wake all send stream wakers.
    pub fn wake_all_send(&mut self) {
        for (_, waker) in self.send_wakers.drain() {
            waker.wake();
        }
    }

    /// Wake all receive stream wakers.
    pub fn wake_all_recv(&mut self) {
        for (_, waker) in self.recv_wakers.drain() {
            waker.wake();
        }
    }
}
