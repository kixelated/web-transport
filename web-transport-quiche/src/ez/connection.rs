use std::sync::Arc;
use std::{
    future::poll_fn,
    ops::Deref,
    sync::{
        atomic::{self, AtomicU64},
        Mutex,
    },
    task::{Poll, Waker},
};
use thiserror::Error;
use tokio_quiche::quiche;

use super::{DriverWakeup, Lock, RecvState, RecvStream, SendState, SendStream, StreamId};

// "conndrop" in ascii; if you see this then close(code)
// decimal: 8029476563109179248
const DROP_CODE: u64 = 0x6F6E6E6464726F70;

/// An errors returned by [`Session`], split based on if they are underlying QUIC errors or WebTransport errors.
#[derive(Clone, Error, Debug)]
pub enum ConnectionError {
    #[error("quiche error: {0}")]
    Quiche(#[from] quiche::Error),

    #[error("remote CONNECTION_CLOSE: code={0} reason={1}")]
    Remote(u64, String),

    #[error("local CONNECTION_CLOSE: code={0} reason={1}")]
    Local(u64, String),

    /// All Connection references were dropped without an explicit close.
    #[error("connection dropped")]
    Dropped,

    /// An unknown error occurred in tokio-quiche.
    #[error("unknown error: {0}")]
    Unknown(String),
}

#[derive(Default)]
struct ConnectionCloseState {
    err: Option<ConnectionError>,
    wakers: Vec<Waker>,
}

#[derive(Clone, Default)]
pub(crate) struct ConnectionClosed {
    state: Arc<Mutex<ConnectionCloseState>>,
}

impl ConnectionClosed {
    pub fn abort(&self, err: ConnectionError) -> Vec<Waker> {
        let mut state = self.state.lock().unwrap();
        if state.err.is_some() {
            return Vec::new();
        }

        state.err = Some(err);
        return std::mem::take(&mut state.wakers);
    }

    // Blocks until the connection is closed and drained.
    pub fn poll(&self, waker: &Waker) -> Poll<ConnectionError> {
        let mut state = self.state.lock().unwrap();
        if state.err.is_some() {
            return Poll::Ready(state.err.clone().unwrap());
        }

        state.wakers.push(waker.clone());

        Poll::Pending
    }

    pub async fn wait(&self) -> ConnectionError {
        poll_fn(|cx| self.poll(cx.waker())).await
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().err.is_some()
    }
}

// Closes the connection when all references are dropped.
struct ConnectionDrop {
    closed: ConnectionClosed,
}

impl ConnectionDrop {
    pub fn new(closed: ConnectionClosed) -> Self {
        Self { closed }
    }
}

impl Drop for ConnectionDrop {
    fn drop(&mut self) {
        self.closed.abort(ConnectionError::Local(
            DROP_CODE,
            "connection dropped".to_string(),
        ));
    }
}

#[derive(Clone)]
pub struct Connection {
    inner: Arc<tokio_quiche::QuicConnection>,

    accept_bi: flume::Receiver<(SendStream, RecvStream)>,
    accept_uni: flume::Receiver<RecvStream>,

    open_bi: flume::Sender<(Lock<SendState>, Lock<RecvState>)>,
    open_uni: flume::Sender<Lock<SendState>>,

    next_uni: Arc<AtomicU64>,
    next_bi: Arc<AtomicU64>,

    send_wakeup: Lock<DriverWakeup>,
    recv_wakeup: Lock<DriverWakeup>,

    closed_local: ConnectionClosed,
    closed_remote: ConnectionClosed,

    #[allow(dead_code)]
    drop: Arc<ConnectionDrop>,
}

impl Connection {
    pub(crate) fn new(
        inner: tokio_quiche::QuicConnection,
        server: bool,
        accept_bi: flume::Receiver<(SendStream, RecvStream)>,
        accept_uni: flume::Receiver<RecvStream>,
        open_bi: flume::Sender<(Lock<SendState>, Lock<RecvState>)>,
        open_uni: flume::Sender<Lock<SendState>>,
        send_wakeup: Lock<DriverWakeup>,
        recv_wakeup: Lock<DriverWakeup>,
        closed_local: ConnectionClosed,
        closed_remote: ConnectionClosed,
    ) -> Self {
        let next_uni = match server {
            true => StreamId::SERVER_UNI,
            false => StreamId::CLIENT_UNI,
        };
        let next_bi = match server {
            true => StreamId::SERVER_BI,
            false => StreamId::CLIENT_BI,
        };

        let drop = Arc::new(ConnectionDrop::new(closed_local.clone()));

        Self {
            inner: Arc::new(inner),
            accept_bi,
            accept_uni,
            open_bi,
            open_uni,
            next_uni: Arc::new(next_uni.into()),
            next_bi: Arc::new(next_bi.into()),
            send_wakeup,
            recv_wakeup,
            closed_local,
            closed_remote,
            drop,
        }
    }

    /// Returns the next bidirectional stream created by the peer.
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), ConnectionError> {
        tokio::select! {
            Ok(res) = self.accept_bi.recv_async() => Ok(res),
            res = self.closed() => Err(res),
        }
    }

    /// Returns the next unidirectional stream, if any.
    pub async fn accept_uni(&self) -> Result<RecvStream, ConnectionError> {
        tokio::select! {
            Ok(res) = self.accept_uni.recv_async() => Ok(res),
            res = self.closed() => Err(res),
        }
    }

    /// Create a new bidirectional stream when the peer allows it.
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), ConnectionError> {
        let id = StreamId::from(self.next_bi.fetch_add(4, atomic::Ordering::Relaxed));

        let send = Lock::new(SendState::new(id), "SendState");
        let recv = Lock::new(RecvState::new(id), "RecvState");

        // TODO block until the driver can create the stream
        tokio::select! {
            Ok(_) = self.open_bi.send_async((send.clone(), recv.clone())) => {},
            res = self.closed() => return Err(res),
        };

        let send = SendStream::new(id, send, self.send_wakeup.clone());
        let recv = RecvStream::new(id, recv, self.recv_wakeup.clone());

        Ok((send, recv))
    }

    /// Create a new unidirectional stream when the peer allows it.
    pub async fn open_uni(&self) -> Result<SendStream, ConnectionError> {
        let id = StreamId::from(self.next_uni.fetch_add(4, atomic::Ordering::Relaxed));

        // TODO wait until the driver ACKs
        let state = Lock::new(SendState::new(id), "SendState");
        tokio::select! {
            Ok(_) = self.open_uni.send_async(state.clone()) => {},
            res = self.closed() => return Err(res),
        };

        Ok(SendStream::new(id, state, self.send_wakeup.clone()))
    }

    /// Closes the connection, returning an error if the connection was already closed.
    ///
    /// You should wait until [Self::closed] returns if you wait to ensure the CONNECTION_CLOSED is received.
    /// Otherwise, the close may be lost and the peer will have to wait for a timeout.
    pub fn close(&self, code: u64, reason: &str) {
        let wakers = self
            .closed_local
            .abort(ConnectionError::Local(code, reason.to_string()));

        for waker in wakers {
            waker.wake();
        }
    }

    /// Blocks until the connection is closed by the peer.
    ///
    /// If [Self::close] is called, this will block until the peer acknowledges the close.
    /// This is recommended to avoid tearing down the connection too early.
    pub async fn closed(&self) -> ConnectionError {
        self.closed_remote.wait().await
    }

    /// Returns true if the connection is closed by either side.
    ///
    /// NOTE: This includes local closures, unlike [Self::closed].
    pub fn is_closed(&self) -> bool {
        self.closed_local.is_closed() || self.closed_remote.is_closed()
    }
}

impl Deref for Connection {
    type Target = tokio_quiche::QuicConnection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
