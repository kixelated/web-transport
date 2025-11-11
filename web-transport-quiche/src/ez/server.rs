use futures::ready;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::{poll_fn, Future},
    io::{self, Cursor},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::{
        atomic::{self, AtomicU64},
        Arc, Mutex, MutexGuard,
    },
    task::{Context, Poll, Waker},
};

// Debug wrapper for Arc<Mutex<T>> that prints lock/unlock operations
struct Lock<T> {
    inner: Arc<Mutex<T>>,
    name: &'static str,
}

impl<T> Clone for Lock<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            name: self.name,
        }
    }
}

impl<T> Lock<T> {
    fn new(value: T, name: &'static str) -> Self {
        Self {
            inner: Arc::new(Mutex::new(value)),
            name,
        }
    }

    fn lock(&self) -> LockGuard<'_, T> {
        println!(
            "LOCK: acquiring {} @ {:?}",
            self.name,
            std::thread::current().id()
        );
        let guard = self.inner.lock().unwrap();
        println!(
            "LOCK: acquired {} @ {:?}",
            self.name,
            std::thread::current().id()
        );
        LockGuard {
            guard,
            name: self.name,
        }
    }
}

struct LockGuard<'a, T> {
    guard: MutexGuard<'a, T>,
    name: &'static str,
}

impl<'a, T> Drop for LockGuard<'a, T> {
    fn drop(&mut self) {
        println!(
            "LOCK: dropping {} @ {:?}",
            self.name,
            std::thread::current().id()
        );
    }
}

impl<'a, T> Deref for LockGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'a, T> DerefMut for LockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::mpsc,
    task::JoinSet,
};
#[cfg(not(target_os = "linux"))]
use tokio_quiche::socket::SocketCapabilities;
use tokio_quiche::{
    buf_factory::{BufFactory, PooledBuf},
    quic::{HandshakeInfo, SimpleConnectionIdGenerator},
    quiche::{self, Shutdown},
    settings::{Hooks, QuicSettings, TlsCertificatePaths},
    socket::QuicListener,
};

pub use tokio_quiche::metrics::{DefaultMetrics, Metrics};

use crate::ez::ConnectionError;

use super::{RecvError, SendError};

use tokio_quiche::quic::QuicheConnection;

pub struct ServerBuilder<M: Metrics> {
    listeners: Vec<QuicListener>,
    settings: QuicSettings,
    metrics: M,
}

impl Default for ServerBuilder<DefaultMetrics> {
    fn default() -> Self {
        Self::new(DefaultMetrics::default())
    }
}

impl<M: Metrics> ServerBuilder<M> {
    pub fn new(m: M) -> Self {
        Self {
            listeners: Default::default(),
            settings: QuicSettings::default(),
            metrics: m,
        }
    }

    pub fn with_listeners(mut self, listeners: impl IntoIterator<Item = QuicListener>) -> Self {
        for listener in listeners {
            self.listeners.push(listener);
        }
        self
    }

    pub fn with_sockets(self, sockets: impl IntoIterator<Item = tokio::net::UdpSocket>) -> Self {
        let start = self.listeners.len();

        self.with_listeners(sockets.into_iter().enumerate().map(|(i, socket)| {
            // TODO Modify quiche to add other platform support.
            #[cfg(target_os = "linux")]
            let capabilities = SocketCapabilities::apply_all_and_get_compatibility(&socket);
            #[cfg(not(target_os = "linux"))]
            let capabilities = SocketCapabilities::default();

            QuicListener {
                socket,
                socket_cookie: (start + i) as _,
                capabilities,
            }
        }))
    }

    pub fn with_addr<A: std::net::ToSocketAddrs>(self, addrs: A) -> io::Result<Self> {
        // We use std to avoid async
        let socket = std::net::UdpSocket::bind(addrs)?;
        socket.set_nonblocking(true)?;
        let socket = tokio::net::UdpSocket::from_std(socket)?;
        Ok(self.with_sockets([socket]))
    }

    pub fn with_settings(mut self, settings: QuicSettings) -> Self {
        self.settings = settings;
        self
    }

    // TODO add support for in-memory certs
    pub fn with_certs<'a>(self, tls: TlsCertificatePaths<'a>) -> io::Result<Server<M>> {
        let params =
            tokio_quiche::ConnectionParams::new_server(self.settings, tls, Hooks::default());
        let server = tokio_quiche::listen_with_capabilities(
            self.listeners,
            params,
            SimpleConnectionIdGenerator,
            self.metrics,
        )?;
        Ok(Server::new(server))
    }
}

pub struct Server<M: Metrics = DefaultMetrics> {
    accept: mpsc::Receiver<Connection>,
    // Cancels socket tasks when dropped.
    #[allow(dead_code)]
    tasks: JoinSet<io::Result<()>>,
    _metrics: PhantomData<M>,
}

impl<M: Metrics> Server<M> {
    fn new(sockets: Vec<tokio_quiche::QuicConnectionStream<M>>) -> Self {
        let mut tasks = JoinSet::default();

        let accept = mpsc::channel(sockets.len());

        for socket in sockets {
            let accept = accept.0.clone();
            // TODO close all when one errors
            tasks.spawn(Self::run_socket(socket, accept));
        }

        Self {
            accept: accept.1,
            _metrics: PhantomData,
            tasks,
        }
    }

    async fn run_socket(
        socket: tokio_quiche::QuicConnectionStream<M>,
        accept: mpsc::Sender<Connection>,
    ) -> io::Result<()> {
        let mut rx = socket.into_inner();
        while let Some(initial) = rx.recv().await {
            let initial = initial?;
            println!("accepted initial");

            let accept_bi = flume::unbounded();
            let accept_uni = flume::unbounded();

            let open_bi = flume::bounded(1);
            let open_uni = flume::bounded(1);

            let send_wakeup = Lock::new(SendWakeup::default(), "send_wakeup");
            let recv_wakeup = Lock::new(RecvWakeup::default(), "recv_wakeup");

            let closed_local = ConnectionClosed::default();
            let closed_remote = ConnectionClosed::default();

            let drop = Arc::new(ConnectionDrop {
                closed: closed_local.clone(),
            });

            let session = Driver {
                send: HashMap::new(),
                recv: HashMap::new(),
                buf: BufFactory::get_max_buf(),
                send_wakeup: send_wakeup.clone(),
                recv_wakeup: recv_wakeup.clone(),
                accept_bi: accept_bi.0,
                accept_uni: accept_uni.0,
                open_bi: open_bi.1,
                open_uni: open_uni.1,
                closed_local: closed_local.clone(),
                closed_remote: closed_remote.clone(),
            };

            println!("starting driver");
            let inner = initial.start(session);
            let connection = Connection {
                inner: Arc::new(inner),
                accept_bi: accept_bi.1,
                accept_uni: accept_uni.1,
                open_bi: open_bi.0,
                open_uni: open_uni.0,
                next_uni: Arc::new(StreamId::SERVER_UNI.into()),
                next_bi: Arc::new(StreamId::SERVER_BI.into()),
                send_wakeup,
                recv_wakeup,
                drop,
                closed_local: closed_local.clone(),
                closed_remote: closed_remote.clone(),
            };

            if accept.send(connection).await.is_err() {
                println!("closed");
                return Ok(());
            }
        }

        Ok(())
    }

    pub async fn accept(&mut self) -> Option<Connection> {
        self.accept.recv().await
    }
}

// Streams that need to be flushed to the quiche connection.
#[derive(Default)]
struct SendWakeup {
    streams: HashSet<StreamId>,
    waker: Option<Waker>,
}

impl SendWakeup {
    pub fn waker(&mut self, stream_id: StreamId) -> Option<Waker> {
        if !self.streams.insert(stream_id) {
            println!("already notifying send driver: {:?}", stream_id);
            return None;
        }

        // You should call wake() without holding the lock.
        return self.waker.take();
    }
}

#[derive(Default, Clone)]
struct RecvWakeup {
    streams: HashSet<StreamId>,
    waker: Option<Waker>,
}

impl RecvWakeup {
    pub fn waker(&mut self, stream_id: StreamId) -> Option<Waker> {
        if !self.streams.insert(stream_id) {
            println!("already notifying recv driver: {:?}", stream_id);
            return None;
        }

        return self.waker.take();
    }
}

#[derive(Default)]
struct ConnectionCloseState {
    err: Option<ConnectionError>,
    wakers: Vec<Waker>,
}

#[derive(Clone, Default)]
struct ConnectionClosed {
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
}

// Closes the connection when all references are dropped.
struct ConnectionDrop {
    closed: ConnectionClosed,
}

impl Drop for ConnectionDrop {
    fn drop(&mut self) {
        self.closed.abort(ConnectionError::Dropped);
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

    send_wakeup: Lock<SendWakeup>,
    recv_wakeup: Lock<RecvWakeup>,

    closed_local: ConnectionClosed,
    closed_remote: ConnectionClosed,

    #[allow(dead_code)]
    drop: Arc<ConnectionDrop>,
}

impl Connection {
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
        let id = StreamId(self.next_bi.fetch_add(4, atomic::Ordering::Relaxed));

        let send = Lock::new(SendState::new(id), "SendState");
        let recv = Lock::new(RecvState::new(id), "RecvState");

        // TODO block until the driver can create the stream
        tokio::select! {
            Ok(_) = self.open_bi.send_async((send.clone(), recv.clone())) => {},
            res = self.closed() => return Err(res),
        };

        let send = SendStream {
            id,
            state: send,
            wakeup: self.send_wakeup.clone(),
        };

        let recv = RecvStream {
            id,
            state: recv,
            wakeup: self.recv_wakeup.clone(),
        };

        Ok((send, recv))
    }

    /// Create a new unidirectional stream when the peer allows it.
    pub async fn open_uni(&self) -> Result<SendStream, ConnectionError> {
        let id = StreamId(self.next_uni.fetch_add(4, atomic::Ordering::Relaxed));

        // TODO wait until the driver ACKs
        let state = Lock::new(SendState::new(id), "SendState");
        tokio::select! {
            Ok(_) = self.open_uni.send_async(state.clone()) => {},
            res = self.closed() => return Err(res),
        };

        Ok(SendStream {
            id,
            state,
            wakeup: self.send_wakeup.clone(),
        })
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
}

impl Deref for Connection {
    type Target = tokio_quiche::QuicConnection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

struct Driver {
    send: HashMap<StreamId, Lock<SendState>>,
    recv: HashMap<StreamId, Lock<RecvState>>,

    buf: PooledBuf,

    send_wakeup: Lock<SendWakeup>,
    recv_wakeup: Lock<RecvWakeup>,

    accept_bi: flume::Sender<(SendStream, RecvStream)>,
    accept_uni: flume::Sender<RecvStream>,

    open_bi: flume::Receiver<(Lock<SendState>, Lock<RecvState>)>,
    open_uni: flume::Receiver<Lock<SendState>>,

    closed_local: ConnectionClosed,
    closed_remote: ConnectionClosed,
}

impl Driver {
    fn connected(
        &mut self,
        qconn: &mut QuicheConnection,
        _handshake_info: &HandshakeInfo,
    ) -> Result<(), ConnectionError> {
        // Run poll once to advance any pending operations.
        match self.poll(Waker::noop(), qconn) {
            Poll::Ready(Err(e)) => Err(e),
            _ => Ok(()),
        }
    }

    fn read(&mut self, qconn: &mut QuicheConnection) -> Result<(), ConnectionError> {
        while let Some(stream_id) = qconn.stream_readable_next() {
            let stream_id = StreamId(stream_id);
            println!("stream is readable: {:?}", stream_id);

            if let Some(entry) = self.recv.get_mut(&stream_id) {
                // Wake after dropping the lock to avoid deadlock
                let waker = entry.lock().flush(qconn)?;
                if let Some(waker) = waker {
                    waker.wake();
                }

                continue;
            }

            println!("stream is new: {:?}", stream_id);

            let mut state = RecvState::new(stream_id);
            state.flush(qconn)?; // no waker will be returned

            let state = Lock::new(state, "RecvState");
            self.recv.insert(stream_id, state.clone());
            let recv = RecvStream {
                id: stream_id,
                state,
                wakeup: self.recv_wakeup.clone(),
            };

            if stream_id.is_bi() {
                let mut state = SendState::new(stream_id);
                state.flush(qconn)?; // no waker will be returned

                let state = Lock::new(state, "SendState");
                self.send.insert(stream_id, state.clone());

                let send = SendStream {
                    id: stream_id,
                    state,
                    wakeup: self.send_wakeup.clone(),
                };
                self.accept_bi
                    .send((send, recv))
                    .map_err(|_| ConnectionError::Dropped)?;
            } else {
                self.accept_uni
                    .send(recv)
                    .map_err(|_| ConnectionError::Dropped)?;
            }
        }

        Ok(())
    }

    fn write(&mut self, qconn: &mut QuicheConnection) -> Result<(), ConnectionError> {
        while let Some(stream_id) = qconn.stream_writable_next() {
            let stream_id = StreamId(stream_id);

            println!("stream is writable: {:?}", stream_id);

            if let Some(state) = self.send.get_mut(&stream_id) {
                let waker = state.lock().flush(qconn)?;
                if let Some(waker) = waker {
                    waker.wake();
                }
            } else {
                return Err(quiche::Error::InvalidStreamState(stream_id.0).into());
            }
        }

        Ok(())
    }

    async fn wait(&mut self, qconn: &mut QuicheConnection) -> Result<(), ConnectionError> {
        poll_fn(|cx| self.poll(cx.waker(), qconn)).await
    }

    fn poll(
        &mut self,
        waker: &Waker,
        qconn: &mut QuicheConnection,
    ) -> Poll<Result<(), ConnectionError>> {
        println!("poll");

        if !qconn.is_draining() {
            // Check if the application wants to close the connection.
            if let Poll::Ready(err) = self.closed_local.poll(waker) {
                match err {
                    ConnectionError::Local(code, reason) => {
                        qconn.close(true, code, reason.as_bytes())
                    }
                    ConnectionError::Dropped => qconn.close(true, 0, b"dropped"),
                    ConnectionError::Remote(code, reason) => {
                        // This shouldn't happen, but just echo it back in case.
                        qconn.close(true, code, reason.as_bytes())
                    }
                    ConnectionError::Quiche(e) => qconn.close(true, 500, e.to_string().as_bytes()),
                    ConnectionError::Unknown(reason) => qconn.close(true, 501, reason.as_bytes()),
                }
                .ok();
            }
        }

        // Don't try to do anything during the handshake.
        if !qconn.is_established() {
            return Poll::Pending;
        }

        // We're allowed to process recv messages when the connection is draining.
        {
            let mut recv = self.recv_wakeup.lock();

            // Register our waker for future wakeups.
            recv.waker = Some(waker.clone());

            // Make sure we drop the lock before processing.
            // Otherwise, we can cause a deadlock trying to access multiple locks at once.
            let streams = std::mem::take(&mut recv.streams);
            drop(recv);

            for stream_id in streams {
                if let Some(stream) = self.recv.get_mut(&stream_id) {
                    println!("wakeup for recv {:?}", stream_id);
                    let waker = stream.lock().flush(qconn)?;
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                } else {
                    println!("wakeup for dropped recv stream");
                }
            }
        }

        // Don't try to send/open during the draining or closed state.
        if qconn.is_draining() || qconn.is_closed() {
            return Poll::Pending;
        }

        {
            let mut send = self.send_wakeup.lock();
            send.waker = Some(waker.clone());

            // Make sure we drop the lock before processing.
            // Otherwise, we can cause a deadlock trying to access multiple locks at once.
            let streams = std::mem::take(&mut send.streams);
            drop(send);

            for stream_id in streams {
                if let Some(stream) = self.send.get_mut(&stream_id) {
                    println!("wakeup for send {:?}", stream_id);
                    let waker = stream.lock().flush(qconn)?;
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                } else {
                    println!("wakeup for dropped send stream");
                }
            }
        }

        while qconn.peer_streams_left_bidi() > 0 {
            if let Ok((send, recv)) = self.open_bi.try_recv() {
                self.open_bi(qconn, send, recv)?;
            } else {
                break;
            }
        }

        while qconn.peer_streams_left_uni() > 0 {
            if let Ok(recv) = self.open_uni.try_recv() {
                self.open_uni(qconn, recv)?;
            } else {
                break;
            }
        }

        Poll::Pending
    }

    fn open_bi(
        &mut self,
        qconn: &mut QuicheConnection,
        send: Lock<SendState>,
        recv: Lock<RecvState>,
    ) -> Result<(), ConnectionError> {
        let id = {
            let mut state = send.lock();
            let id = state.id;
            println!("opening send bi: {:?}", state.id);
            qconn.stream_send(state.id.0, &[], false)?;
            let waker = state.flush(qconn)?;
            drop(state);
            if let Some(waker) = waker {
                waker.wake();
            }
            id
        };
        self.send.insert(id, send);

        let id = {
            let mut state = recv.lock();
            let id = state.id;
            let waker = state.flush(qconn)?;
            drop(state);
            if let Some(waker) = waker {
                waker.wake();
            }
            println!("opening recv bi: {:?}", id);
            id
        };
        self.recv.insert(id, recv);

        Ok(())
    }

    fn open_uni(
        &mut self,
        qconn: &mut QuicheConnection,
        send: Lock<SendState>,
    ) -> Result<(), ConnectionError> {
        let id = {
            let mut state = send.lock();
            let id = state.id;
            println!("opening send uni: {:?}", id);
            qconn.stream_send(state.id.0, &[], false)?;
            let waker = state.flush(qconn)?;
            drop(state);
            if let Some(waker) = waker {
                waker.wake();
            }
            id
        };
        self.send.insert(id, send);

        Ok(())
    }

    fn abort(&mut self, err: ConnectionError) {
        let wakers = self.closed_local.abort(err);
        for waker in wakers {
            waker.wake();
        }
    }
}

impl tokio_quiche::ApplicationOverQuic for Driver {
    fn on_conn_established(
        &mut self,
        qconn: &mut QuicheConnection,
        handshake_info: &tokio_quiche::quic::HandshakeInfo,
    ) -> tokio_quiche::QuicResult<()> {
        println!("on_conn_established");

        if let Err(e) = self.connected(qconn, handshake_info) {
            self.abort(e);
        }

        Ok(())
    }

    fn should_act(&self) -> bool {
        // TODO
        true
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.buf
    }

    fn wait_for_data(
        &mut self,
        qconn: &mut QuicheConnection,
    ) -> impl Future<Output = Result<(), tokio_quiche::BoxError>> + Send {
        async {
            if let Err(e) = self.wait(qconn).await {
                self.abort(e.clone());
            }

            Ok(())
        }
    }

    fn process_reads(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        println!("process_reads");

        if let Err(e) = self.read(qconn) {
            self.abort(e);
        }

        Ok(())
    }

    fn process_writes(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        println!("process_writes");

        if let Err(e) = self.write(qconn) {
            self.abort(e);
        }

        Ok(())
    }

    fn on_conn_close<M: Metrics>(
        &mut self,
        qconn: &mut QuicheConnection,
        _metrics: &M,
        connection_result: &tokio_quiche::QuicResult<()>,
    ) {
        let err = if let Poll::Ready(err) = self.closed_local.poll(Waker::noop()) {
            err
        } else if let Some(local) = qconn.local_error() {
            let reason = String::from_utf8_lossy(&local.reason).to_string();
            ConnectionError::Local(local.error_code, reason)
        } else if let Some(peer) = qconn.peer_error() {
            let reason = String::from_utf8_lossy(&peer.reason).to_string();
            ConnectionError::Remote(peer.error_code, reason)
        } else if let Err(err) = connection_result {
            ConnectionError::Unknown(err.to_string())
        } else {
            ConnectionError::Unknown("no error message".to_string())
        };

        // Finally set the remote error once the connection is done.
        let wakers = self.closed_remote.abort(err);
        for waker in wakers {
            waker.wake();
        }
    }
}

struct SendState {
    id: StreamId,

    // The amount of data that is allowed to be written.
    capacity: usize,

    // Data ready to send. (capacity has been subtracted)
    queued: VecDeque<Bytes>,

    // Called by the driver when the stream is writable again.
    blocked: Option<Waker>,

    // send STREAM_FIN
    fin: bool,

    // send RESET_STREAM
    reset: Option<u64>,

    // received
    stop: Option<u64>,

    // received SET_PRIORITY
    priority: Option<u8>,
}

impl SendState {
    pub fn new(id: StreamId) -> Self {
        Self {
            id,
            capacity: 0,
            queued: VecDeque::new(),
            blocked: None,
            fin: false,
            reset: None,
            stop: None,
            priority: None,
        }
    }

    pub fn flush(&mut self, qconn: &mut QuicheConnection) -> quiche::Result<Option<Waker>> {
        if let Some(reset) = self.reset {
            println!("shutting down send bi: {:?} {:?}", self.id, reset);
            assert!(self.blocked.is_none(), "nothing should be blocked");
            qconn.stream_shutdown(self.id.0, Shutdown::Write, reset)?;
            return Ok(None);
        }

        if let Some(priority) = self.priority.take() {
            println!("setting priority: {:?} {:?}", self.id, priority);
            qconn.stream_priority(self.id.0, priority, true)?;
        }

        while let Some(mut chunk) = self.queued.pop_front() {
            println!("sending chunk: {:?} {:?}", self.id, chunk.len());

            let n = match qconn.stream_send(self.id.0, &chunk, false) {
                Ok(n) => n,
                Err(quiche::Error::Done) => 0,
                Err(quiche::Error::StreamStopped(code)) => {
                    self.stop = Some(code);
                    return Ok(self.blocked.take());
                }
                Err(e) => return Err(e.into()),
            };

            println!("sent chunk: {:?} {:?}", self.id, n);
            self.capacity -= n;
            println!("capacity after sending: {:?} {:?}", self.id, self.capacity);

            if n < chunk.len() {
                println!("queued remainder: {:?} {:?}", self.id, chunk.len() - n);

                self.queued.push_front(chunk.split_off(n));

                // Register a `stream_writable_next` callback when at least one byte is ready to send.
                qconn.stream_writable(self.id.0, 1)?;

                break;
            }
        }

        if self.queued.is_empty() {
            if self.fin {
                println!("sending fin: {:?}", self.id);
                assert!(self.blocked.is_none(), "nothing should be blocked");
                qconn.stream_send(self.id.0, &[], true)?;
                return Ok(None);
            }
        }

        self.capacity = match qconn.stream_capacity(self.id.0) {
            Ok(capacity) => capacity,
            Err(quiche::Error::StreamStopped(code)) => {
                self.stop = Some(code);
                println!("waking blocked for stop: {:?}", self.id);
                return Ok(self.blocked.take());
            }
            Err(e) => return Err(e.into()),
        };
        println!("setting capacity: {:?} {:?}", self.id, self.capacity);

        if self.capacity > 0 {
            return Ok(self.blocked.take());
        }

        Ok(None)
    }
}

pub struct SendStream {
    id: StreamId,
    state: Lock<SendState>,

    // Used to wake up the driver when the stream is writable.
    wakeup: Lock<SendWakeup>,
}

impl SendStream {
    pub fn id(&self) -> StreamId {
        self.id
    }

    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, SendError> {
        let mut buf = Cursor::new(buf);
        poll_fn(|cx| self.poll_write_buf(cx, &mut buf)).await
    }

    // Write some of the buffer to the stream, advancing the internal position.
    // Returns the number of bytes written for convenience.
    fn poll_write_buf<B: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, SendError>> {
        println!("poll_write_buf: {:?} {:?}", self.id, buf.remaining());

        let mut state = self.state.lock();
        if let Some(stop) = state.stop {
            return Poll::Ready(Err(SendError::Stop(stop)));
        }

        if state.capacity == 0 {
            state.blocked = Some(cx.waker().clone());
            println!("blocking for capacity: {:?}", self.id);
            return Poll::Pending;
        }

        let n = state.capacity.min(buf.remaining());
        println!("writing {:?} bytes: {:?} {:?}", n, self.id, buf.remaining());

        // NOTE: Avoids a copy when Buf is Bytes.
        let chunk = buf.copy_to_bytes(n);

        state.capacity -= chunk.len();
        state.queued.push_back(chunk);

        // Tell the driver that there's at least one byte ready to send.
        // NOTE: We only do this on the first chunk to avoid spurious wakeups.
        if state.queued.len() == 1 {
            drop(state);

            let waker = self.wakeup.lock().waker(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }
        }

        Poll::Ready(Ok(n))
    }

    /// Write all of the slice to the stream.
    pub async fn write_all(&mut self, mut buf: &[u8]) -> Result<(), SendError> {
        while !buf.is_empty() {
            let n = self.write(buf).await?;
            buf = &buf[n..];
        }
        Ok(())
    }

    /// Write some of the buffer to the stream, advancing the internal position.
    ///
    /// Returns the number of bytes written for convenience.
    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, SendError> {
        poll_fn(|cx| self.poll_write_buf(cx, buf)).await
    }

    /// Write the entire buffer to the stream, advancing the internal position.
    pub async fn write_buf_all<B: Buf>(&mut self, buf: &mut B) -> Result<(), SendError> {
        while buf.has_remaining() {
            self.write_buf(buf).await?;
        }
        Ok(())
    }

    pub fn finish(self) {
        self.state.lock().fin = true;

        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub fn reset(self, code: u64) {
        self.state.lock().reset = Some(code);

        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub fn set_priority(&mut self, priority: u8) {
        self.state.lock().priority = Some(priority);

        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        let mut state = self.state.lock();

        if !state.fin && state.reset.is_none() {
            state.reset = Some(0);
            drop(state);

            let waker = self.wakeup.lock().waker(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let mut buf = Cursor::new(buf);
        match ready!(self.poll_write_buf(cx, &mut buf)) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e.to_string()))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        // Flushing happens automatically via the driver
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        // We purposely don't implement this; use finish() instead because it takes self.
        Poll::Ready(Ok(()))
    }
}

struct RecvState {
    id: StreamId,

    // Data that has been read and needs to be returned to the application.
    queued: VecDeque<Bytes>,

    // The amount of data that should be queued.
    max: usize,

    // The driver wakes up the application when data is available.
    blocked: Option<Waker>,

    // Set when STREAM_FIN
    fin: bool,

    // Set when RESET_STREAM is received
    reset: Option<u64>,

    // Set when STOP_SENDING is sent
    stop: Option<u64>,

    // Buffer for reading data.
    buf: BytesMut,

    // The size of the buffer doubles each time until it reaches the maximum size.
    buf_capacity: usize,
}

impl RecvState {
    pub fn new(id: StreamId) -> Self {
        Self {
            id,
            queued: Default::default(),
            max: 0,
            blocked: None,
            fin: false,
            reset: None,
            stop: None,
            buf: BytesMut::with_capacity(64),
            buf_capacity: 64,
        }
    }

    pub fn flush(&mut self, qconn: &mut QuicheConnection) -> quiche::Result<Option<Waker>> {
        if let Some(code) = self.reset {
            println!("already reset: {:?} {:?}", self.id, code);
            println!("TODO clean up");
            return Ok(self.blocked.take());
        }

        if let Some(stop) = self.stop {
            println!("shutting down recv: {:?} {:?}", self.id, stop);
            qconn.stream_shutdown(self.id.0, Shutdown::Read, stop)?;
            assert!(self.blocked.is_none(), "nothing should be blocked");
            return Ok(None);
        }

        let mut wakeup = false;

        while self.max > 0 {
            if self.buf.capacity() == 0 {
                // TODO get the readable size in Quiche so we can use that instead of guessing.
                self.buf_capacity = (self.buf_capacity * 2).min(32 * 1024);
                println!("reserving buffer: {:?} {:?}", self.id, self.buf_capacity);
                self.buf.reserve(self.buf_capacity);
            }

            // We don't actually use the buffer.len() because we immediately call split_to after reading.
            assert!(
                self.buf.is_empty(),
                "buffer should always be empty (but have capacity)"
            );

            // Do some unsafe to avoid zeroing the buffer.
            let buf: &mut [u8] = unsafe { std::mem::transmute(self.buf.spare_capacity_mut()) };
            let n = buf.len().min(self.max);

            match qconn.stream_recv(self.id.0, &mut buf[..n]) {
                Ok((n, done)) => {
                    println!("received chunk: {:?} {:?} {:?}", self.id, n, done);
                    // Advance the buffer by the number of bytes read.
                    unsafe { self.buf.set_len(self.buf.len() + n) };

                    // Then split the buffer and push the front to the queue.
                    self.queued.push_back(self.buf.split_to(n).freeze());
                    self.max -= n;

                    wakeup = true;

                    println!("capacity after receiving: {:?} {:?}", self.id, self.max);

                    if done {
                        println!("setting fin: {:?}", self.id);
                        self.fin = true;
                        return Ok(self.blocked.take());
                    }
                }
                Err(quiche::Error::Done) => {
                    if qconn.stream_finished(self.id.0) {
                        self.fin = true;
                        println!("waking blocked for FIN: {:?}", self.id);
                        return Ok(self.blocked.take());
                    }
                    break;
                }
                Err(quiche::Error::StreamReset(code)) => {
                    println!("stream reset: {:?} {:?}", self.id, code);
                    self.reset = Some(code);
                    println!("waking blocked for stream reset: {:?}", self.id);
                    return Ok(self.blocked.take());
                }
                Err(e) => return Err(e.into()),
            }
        }

        if wakeup {
            println!("waking blocked for received chunk: {:?}", self.id);
            Ok(self.blocked.take())
        } else {
            Ok(None)
        }
    }
}

pub struct RecvStream {
    id: StreamId,
    state: Lock<RecvState>,
    wakeup: Lock<RecvWakeup>,
}

impl RecvStream {
    pub fn id(&self) -> StreamId {
        self.id
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, RecvError> {
        Ok(self.read_chunk(buf.len()).await?.map(|chunk| {
            buf[..chunk.len()].copy_from_slice(&chunk);
            chunk.len()
        }))
    }

    pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, RecvError> {
        poll_fn(|cx| self.poll_read_chunk(cx, max)).await
    }

    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, RecvError>> {
        println!("poll_read_chunk: {:?} {:?}", self.id, max);
        let mut state = self.state.lock();

        if let Some(reset) = state.reset {
            println!("returning reset: {:?} {:?}", self.id, reset);
            return Poll::Ready(Err(RecvError::Reset(reset)));
        }

        if let Some(mut chunk) = state.queued.pop_front() {
            if chunk.len() > max {
                let remain = chunk.split_off(max);
                state.queued.push_front(remain);
            }
            println!("returning chunk: {:?} {:?}", self.id, chunk.len());
            return Poll::Ready(Ok(Some(chunk)));
        }

        if state.fin {
            println!("returning fin: {:?}", self.id);
            return Poll::Ready(Ok(None));
        }

        // We'll return None if FIN, otherwise return an empty chunk.
        if max == 0 {
            return Poll::Ready(Ok(Some(Bytes::new())));
        }

        state.max = max;

        state.blocked = Some(cx.waker().clone());
        println!("blocking for read: {:?}", self.id);

        // Drop the state lock before acquiring wakeup lock to avoid deadlock
        drop(state);

        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }

        Poll::Pending
    }

    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<(), RecvError> {
        println!("!!! reading buf: {:?} !!!", self.id);

        match self
            .read(unsafe { std::mem::transmute(buf.chunk_mut()) })
            .await?
        {
            Some(n) => {
                unsafe { buf.advance_mut(n) };
                println!("!!! read buf: {:?} {:?} !!!", self.id, n);
                Ok(())
            }
            None => Err(RecvError::Closed),
        }
    }

    pub async fn read_all(&mut self) -> Result<Bytes, RecvError> {
        let mut buf = BytesMut::new();
        println!("!!! reading all: {:?} !!!", self.id);
        loop {
            match self.read_buf(&mut buf).await {
                Ok(()) => continue,
                Err(RecvError::Closed) => break,
                Err(e) => return Err(e),
            }
        }

        println!("!!! read all: {:?} {:?} !!!", self.id, buf.len());

        Ok(buf.freeze())
    }

    pub fn stop(self, code: u64) {
        self.state.lock().stop = Some(code);
        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        let mut state = self.state.lock();

        if !state.fin && state.stop.is_none() {
            state.stop = Some(0);
            // Avoid two locks at once.
            drop(state);

            let waker = self.wakeup.lock().waker(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match ready!(self.poll_read_chunk(cx, buf.remaining())) {
            Ok(Some(chunk)) => buf.put_slice(&chunk),
            Ok(None) => {}
            Err(e) => return Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e.to_string()))),
        };
        Poll::Ready(Ok(()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

impl StreamId {
    // The first stream IDs
    pub const CLIENT_BI: StreamId = StreamId(0);
    pub const SERVER_BI: StreamId = StreamId(1);
    pub const CLIENT_UNI: StreamId = StreamId(2);
    pub const SERVER_UNI: StreamId = StreamId(3);

    pub fn is_uni(&self) -> bool {
        // 2, 3, 6, 7, etc
        self.0 & 0b10 == 0b10
    }

    pub fn is_bi(&self) -> bool {
        !self.is_uni()
    }

    pub fn is_server(&self) -> bool {
        // 1, 3, 5, 7, etc
        self.0 & 0b01 == 0b01
    }

    pub fn is_client(&self) -> bool {
        !self.is_server()
    }

    pub fn increment(&mut self) -> StreamId {
        let id = self.clone();
        self.0 += 4;
        id
    }
}

impl From<StreamId> for AtomicU64 {
    fn from(id: StreamId) -> Self {
        AtomicU64::new(id.0)
    }
}

impl From<StreamId> for u64 {
    fn from(id: StreamId) -> Self {
        id.0
    }
}

impl From<u64> for StreamId {
    fn from(id: u64) -> Self {
        StreamId(id)
    }
}
