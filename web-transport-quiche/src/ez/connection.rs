use std::sync::Arc;
use std::{
    future::poll_fn,
    ops::Deref,
    sync::Mutex,
    task::{Poll, Waker},
};
use thiserror::Error;
use tokio_quiche::quiche;

use crate::ez::DriverState;

use super::{Lock, RecvStream, SendStream};

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
struct ConnectionClosedState {
    err: Option<ConnectionError>,
    wakers: Vec<Waker>,
}

#[derive(Clone, Default)]
pub(super) struct ConnectionClosed {
    state: Arc<Mutex<ConnectionClosedState>>,
}

impl ConnectionClosed {
    pub fn abort(&self, err: ConnectionError) -> Vec<Waker> {
        let mut state = self.state.lock().unwrap();
        if state.err.is_some() {
            return Vec::new();
        }

        state.err = Some(err);
        std::mem::take(&mut state.wakers)
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

    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().err.is_some()
    }
}

// Closes the connection when all references are dropped.
struct ConnectionClose {
    driver: Lock<DriverState>,
}

impl ConnectionClose {
    pub fn new(driver: Lock<DriverState>) -> Self {
        Self { driver }
    }

    pub fn close(&self, err: ConnectionError) {
        let wakers = self.driver.lock().close(err);

        for waker in wakers {
            waker.wake();
        }
    }

    pub async fn wait(&self) -> ConnectionError {
        poll_fn(|cx| self.driver.lock().closed(cx.waker())).await
    }

    pub fn is_closed(&self) -> bool {
        self.driver.lock().is_closed()
    }
}

impl Drop for ConnectionClose {
    fn drop(&mut self) {
        self.close(ConnectionError::Dropped);
    }
}

#[derive(Clone)]
pub struct Connection {
    inner: Arc<tokio_quiche::QuicConnection>,

    // Unbounded
    accept_bi: flume::Receiver<(SendStream, RecvStream)>,
    accept_uni: flume::Receiver<RecvStream>,

    driver: Lock<DriverState>,

    // Held in an Arc so we can use Drop when all references are dropped.
    close: Arc<ConnectionClose>,
}

impl Connection {
    pub(super) fn new(
        conn: tokio_quiche::QuicConnection,
        driver: Lock<DriverState>,
        accept_bi: flume::Receiver<(SendStream, RecvStream)>,
        accept_uni: flume::Receiver<RecvStream>,
    ) -> Self {
        let close = Arc::new(ConnectionClose::new(driver.clone()));

        Self {
            inner: Arc::new(conn),
            accept_bi,
            accept_uni,
            driver,
            close,
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
        let (wakeup, id, send, recv) = poll_fn(|cx| self.driver.lock().open_bi(cx.waker())).await?;
        if let Some(wakeup) = wakeup {
            wakeup.wake();
        }

        let send = SendStream::new(id, send, self.driver.clone());
        let recv = RecvStream::new(id, recv, self.driver.clone());

        Ok((send, recv))
    }

    /// Create a new unidirectional stream when the peer allows it.
    pub async fn open_uni(&self) -> Result<SendStream, ConnectionError> {
        let (wakeup, id, send) = poll_fn(|cx| self.driver.lock().open_uni(cx.waker())).await?;
        if let Some(wakeup) = wakeup {
            wakeup.wake();
        }

        let send = SendStream::new(id, send, self.driver.clone());
        Ok(send)
    }

    /// Closes the connection, returning an error if the connection was already closed.
    ///
    /// You should wait until [Self::closed] returns if you wait to ensure the CONNECTION_CLOSED is received.
    /// Otherwise, the close may be lost and the peer will have to wait for a timeout.
    pub fn close(&self, code: u64, reason: &str) {
        self.close
            .close(ConnectionError::Local(code, reason.to_string()));
    }

    /// Blocks until the connection is closed by the peer.
    ///
    /// If [Self::close] is called, this will block until the peer acknowledges the close.
    /// This is recommended to avoid tearing down the connection too early.
    pub async fn closed(&self) -> ConnectionError {
        self.close.wait().await
    }

    /// Returns true if the connection is closed by either side.
    ///
    /// NOTE: This includes local closures, unlike [Self::closed].
    pub fn is_closed(&self) -> bool {
        self.close.is_closed()
    }
}

impl Deref for Connection {
    type Target = tokio_quiche::QuicConnection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
