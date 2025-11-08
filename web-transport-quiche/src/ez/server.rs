use futures::ready;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    io,
    marker::PhantomData,
    ops::Deref,
    pin::Pin,
    sync::{
        atomic::{self, AtomicU64},
        Arc, Mutex,
    },
    task::{Context, Poll},
};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{futures::OwnedNotified, mpsc, watch, Notify},
    task::JoinSet,
};
#[cfg(not(target_os = "linux"))]
use tokio_quiche::socket::SocketCapabilities;
use tokio_quiche::{
    buf_factory::{BufFactory, PooledBuf},
    quic::SimpleConnectionIdGenerator,
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
    _metrics: PhantomData<M>,
}

impl<M: Metrics> Server<M> {
    fn new(sockets: Vec<tokio_quiche::QuicConnectionStream<M>>) -> Self {
        let mut tasks = JoinSet::default();

        let accept = mpsc::channel(sockets.len());

        for socket in sockets {
            // TODO close all when one errors
            tasks.spawn(Self::run_socket(socket, accept.0.clone()));
        }

        Self {
            accept: accept.1,
            _metrics: PhantomData,
        }
    }

    async fn run_socket(
        socket: tokio_quiche::QuicConnectionStream<M>,
        accept: mpsc::Sender<Connection>,
    ) -> io::Result<()> {
        let mut rx = socket.into_inner();
        while let Some(initial) = rx.recv().await {
            let accept_bi = flume::unbounded();
            let accept_uni = flume::unbounded();
            let open_bi = flume::bounded(16);
            let open_uni = flume::bounded(16);
            let closed = watch::channel(None);

            let session = Driver::new(
                accept_bi.0,
                accept_uni.0,
                open_bi.1,
                open_uni.1,
                closed.clone(),
            );
            let inner = initial?.start(session);
            let connection = Connection {
                inner: Arc::new(inner),
                accept_bi: accept_bi.1,
                accept_uni: accept_uni.1,
                open_bi: open_bi.0,
                open_uni: open_uni.0,
                next_uni: Arc::new(StreamId::SERVER_UNI.into()),
                next_bi: Arc::new(StreamId::SERVER_BI.into()),
                wakeup: Default::default(),
                closed: closed.0,
            };

            if accept.send(connection).await.is_err() {
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
struct WakeupState {
    send: HashSet<StreamId>,
    recv: HashSet<StreamId>,
    notify: Arc<Notify>,
}

impl WakeupState {
    pub fn send(&mut self, stream_id: StreamId) {
        if self.send.insert(stream_id) {
            self.notify.notify_waiters();
        }
    }

    pub fn recv(&mut self, stream_id: StreamId) {
        if self.recv.insert(stream_id) {
            self.notify.notify_waiters();
        }
    }
}

#[derive(Clone)]
pub struct Connection {
    inner: Arc<tokio_quiche::QuicConnection>,

    accept_bi: flume::Receiver<(SendStream, RecvStream)>,
    accept_uni: flume::Receiver<RecvStream>,

    open_bi: flume::Sender<(Arc<Mutex<SendState>>, Arc<Mutex<RecvState>>)>,
    open_uni: flume::Sender<Arc<Mutex<SendState>>>,

    next_uni: Arc<AtomicU64>,
    next_bi: Arc<AtomicU64>,

    closed: watch::Sender<Option<ConnectionError>>,

    wakeup: Arc<Mutex<WakeupState>>,
}

impl Connection {
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), ConnectionError> {
        tokio::select! {
            Ok(res) = self.accept_bi.recv_async() => Ok(res),
            err = self.closed() => Err(err),
        }
    }

    pub async fn accept_uni(&self) -> Result<RecvStream, ConnectionError> {
        tokio::select! {
            Ok(res) = self.accept_uni.recv_async() => Ok(res),
            err = self.closed() => Err(err),
        }
    }

    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), ConnectionError> {
        let id = StreamId(self.next_bi.fetch_add(4, atomic::Ordering::Relaxed));

        let send = Arc::new(Mutex::new(SendState::new(id)));
        let recv = Arc::new(Mutex::new(RecvState::new(id)));

        tokio::select! {
            Ok(()) = self.open_bi.send_async((send.clone(), recv.clone())) => {},
            err = self.closed() => return Err(err),
        };

        let send = SendStream {
            id,
            state: send,
            wakeup: self.wakeup.clone(),
        };

        let recv = RecvStream {
            id,
            state: recv,
            wakeup: self.wakeup.clone(),
        };

        Ok((send, recv))
    }

    pub async fn open_uni(&self) -> Result<SendStream, ConnectionError> {
        let id = StreamId(self.next_uni.fetch_add(4, atomic::Ordering::Relaxed));

        let state = Arc::new(Mutex::new(SendState::new(id)));
        tokio::select! {
            Ok(()) = self.open_uni.send_async(state.clone()) => {},
            err = self.closed() => return Err(err),
        };

        Ok(SendStream {
            id,
            state,
            wakeup: self.wakeup.clone(),
        })
    }

    pub fn close(self, code: u64, reason: &str) {
        self.closed
            .send_replace(Some(ConnectionError::Closed(code, reason.to_string())));
    }

    pub async fn closed(&self) -> ConnectionError {
        self.closed
            .subscribe()
            .wait_for(|err| err.is_some())
            .await
            .unwrap()
            .clone()
            .unwrap()
    }
}

impl Deref for Connection {
    type Target = tokio_quiche::QuicConnection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

struct Driver {
    send: HashMap<StreamId, Arc<Mutex<SendState>>>,
    recv: HashMap<StreamId, Arc<Mutex<RecvState>>>,

    buf: PooledBuf,

    wakeup: Arc<Mutex<WakeupState>>,

    accept_bi: flume::Sender<(SendStream, RecvStream)>,
    accept_uni: flume::Sender<RecvStream>,

    open_bi: flume::Receiver<(Arc<Mutex<SendState>>, Arc<Mutex<RecvState>>)>,
    open_uni: flume::Receiver<Arc<Mutex<SendState>>>,

    closed: (
        watch::Sender<Option<ConnectionError>>,
        watch::Receiver<Option<ConnectionError>>,
    ),
}

impl Driver {
    fn new(
        accept_bi: flume::Sender<(SendStream, RecvStream)>,
        accept_uni: flume::Sender<RecvStream>,
        open_bi: flume::Receiver<(Arc<Mutex<SendState>>, Arc<Mutex<RecvState>>)>,
        open_uni: flume::Receiver<Arc<Mutex<SendState>>>,
        closed: (
            watch::Sender<Option<ConnectionError>>,
            watch::Receiver<Option<ConnectionError>>,
        ),
    ) -> Self {
        Self {
            send: HashMap::new(),
            recv: HashMap::new(),
            buf: BufFactory::get_max_buf(),
            wakeup: Default::default(),
            accept_bi,
            accept_uni,
            open_bi,
            open_uni,
            closed,
        }
    }

    async fn wait(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        loop {
            // Notified is a gross API.
            // We need this block because the compiler isn't smart enough to detect drop(state).
            let notified = {
                let mut state = self.wakeup.lock().unwrap();

                if !state.send.is_empty() || !state.recv.is_empty() {
                    for stream_id in state.send.drain() {
                        if let Some(stream) = self.send.get_mut(&stream_id) {
                            stream.lock().unwrap().flush(qconn)?;
                        }
                    }

                    for stream_id in state.recv.drain() {
                        if let Some(stream) = self.recv.get_mut(&stream_id) {
                            stream.lock().unwrap().flush(qconn)?;
                        }
                    }

                    // Let the QUIC stack do its thing.
                    return Ok(());
                }

                Notify::notified_owned(state.notify.clone())
            };

            tokio::select! {
                _ = notified => {},
                Ok((send, recv)) = self.open_bi.recv_async() => {
                    let id = {
                        let mut state = send.lock().unwrap();
                        state.flush(qconn)?;
                        state.id
                    };
                    self.send.insert(id, send);

                    let id = {
                        let mut state = recv.lock().unwrap();
                        state.flush(qconn)?;
                        state.id
                    };
                    self.recv.insert(id, recv);
                }
                Ok(send) = self.open_uni.recv_async() => {
                    let id = {
                        let mut state = send.lock().unwrap();
                        state.flush(qconn)?;
                        state.id
                    };
                    self.send.insert(id, send);
                }
                Ok(closed) = self.closed.1.wait_for(|err| err.is_some()) => {
                    match closed.as_ref().unwrap() {
                        ConnectionError::Closed(code, reason) => qconn.close(true, *code, reason.as_bytes())?,
                        ConnectionError::Quiche(_) => qconn.close(true, 500, b"internal server error")?,
                    }
                }
            }
        }
    }
}

impl tokio_quiche::ApplicationOverQuic for Driver {
    fn on_conn_established(
        &mut self,
        qconn: &mut QuicheConnection,
        _handshake_info: &tokio_quiche::quic::HandshakeInfo,
    ) -> tokio_quiche::QuicResult<()> {
        // I don't think we need to do anything with writable streams here?
        self.process_reads(qconn)
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
    ) -> impl Future<Output = tokio_quiche::QuicResult<()>> + Send {
        self.wait(qconn)
    }

    fn process_reads(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        while let Some(stream_id) = qconn.stream_readable_next() {
            let stream_id = StreamId(stream_id);

            if let Some(entry) = self.recv.get_mut(&stream_id) {
                entry.lock().unwrap().flush(qconn)?;
                continue;
            }

            let mut state = RecvState::new(stream_id);
            state.flush(qconn)?;

            let state = Arc::new(Mutex::new(state));
            self.recv.insert(stream_id, state.clone());
            let recv = RecvStream {
                id: stream_id,
                state,
                wakeup: self.wakeup.clone(),
            };

            if stream_id.is_bi() {
                let mut state = SendState::new(stream_id);
                state.flush(qconn)?;

                let state = Arc::new(Mutex::new(state));
                self.send.insert(stream_id, state.clone());

                let send = SendStream {
                    id: stream_id,
                    state,
                    wakeup: self.wakeup.clone(),
                };
                self.accept_bi.send((send, recv))?;
            } else {
                self.accept_uni.send(recv)?;
            }
        }

        Ok(())
    }

    fn process_writes(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        while let Some(stream_id) = qconn.stream_writable_next() {
            let stream_id = StreamId(stream_id);

            if let Some(state) = self.send.get_mut(&stream_id) {
                state.lock().unwrap().flush(qconn)?;
            } else {
                return Err(quiche::Error::InvalidStreamState(stream_id.0).into());
            }
        }

        Ok(())
    }
}

struct SendState {
    id: StreamId,

    // The amount of data that is allowed to be written.
    capacity: usize,

    // Data ready to send. (capacity has been subtracted)
    queued: VecDeque<Bytes>,

    // Called by the driver when the stream is writable again.
    writable: Arc<Notify>,

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
            writable: Arc::new(Notify::new()),
            fin: false,
            reset: None,
            stop: None,
            priority: None,
        }
    }

    pub fn flush(&mut self, qconn: &mut QuicheConnection) -> quiche::Result<()> {
        if let Some(reset) = self.reset {
            qconn.stream_shutdown(self.id.0, Shutdown::Write, reset)?;
            return Ok(());
        }

        if let Some(priority) = self.priority.take() {
            qconn.stream_priority(self.id.0, priority, true)?;
        }

        while let Some(mut chunk) = self.queued.pop_front() {
            // We call stream_writable first to make sure we register a callback when the stream is writable.
            match qconn.stream_writable(self.id.0, 1) {
                Ok(true) => {
                    let n = qconn.stream_send(self.id.0, &chunk, false)?;
                    if n < chunk.len() {
                        self.queued.push_front(chunk.split_off(n));
                    }
                }
                Ok(false) => self.queued.push_front(chunk),
                Err(quiche::Error::StreamStopped(code)) => {
                    self.stop = Some(code);
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };

            // Can't write any more data
            break;
        }

        if self.queued.is_empty() {
            if self.fin {
                qconn.stream_send(self.id.0, &[], true)?;
                return Ok(());
            }

            self.capacity = qconn.stream_capacity(self.id.0)?;
        }

        Ok(())
    }
}

enum SendResult {
    Success(usize),
    Blocked(OwnedNotified),
}

pub struct SendStream {
    id: StreamId,
    state: Arc<Mutex<SendState>>,

    // Used to wake up the driver when the stream is writable.
    wakeup: Arc<Mutex<WakeupState>>,
}

impl SendStream {
    pub fn id(&self) -> StreamId {
        self.id
    }

    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, SendError> {
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            match self.try_write(buf)? {
                SendResult::Success(n) => return Ok(n),
                SendResult::Blocked(notified) => notified.await,
            }
        }
    }

    // Try to write the given buffer to the stream.
    // Returns the number of bytes written and a notification to wake up the driver when the stream is writable again.
    fn try_write(&mut self, buf: &[u8]) -> Result<SendResult, SendError> {
        let mut state = self.state.lock().unwrap();
        if let Some(stop) = state.stop {
            return Err(SendError::Stop(stop));
        }

        if state.capacity == 0 {
            let notified = state.writable.clone().notified_owned();
            return Ok(SendResult::Blocked(notified));
        }

        let n = buf.len().min(state.capacity);

        if let Some(back) = state.queued.pop_back() {
            // Try appending to the existing buffer instead of allocating.
            match back.try_into_mut() {
                Ok(mut back) if back.remaining_mut() >= n => {
                    back.copy_from_slice(&buf[..n]);
                    state.capacity -= n;
                    return Ok(SendResult::Success(n));
                }
                Ok(back) => state.queued.push_back(back.freeze()),
                Err(back) => state.queued.push_back(back),
            }
        } else {
            // Tell the driver that there's at least one byte ready to send.
            // NOTE: We only do this when state.queued.is_empty() as an optimization.
            self.wakeup.lock().unwrap().send(self.id);
        }

        state.queued.push_back(Bytes::copy_from_slice(&buf[..n]));
        state.capacity -= n;

        return Ok(SendResult::Success(n));
    }

    pub async fn write_chunk(&mut self, mut buf: Bytes) -> Result<(), SendError> {
        while !buf.is_empty() {
            let mut state = self.state.lock().unwrap();
            if let Some(stop) = state.stop {
                return Err(SendError::Stop(stop));
            }

            if state.capacity == 0 {
                let notified = state.writable.clone().notified_owned();
                drop(state);
                notified.await;
                continue;
            }

            let chunk = buf.split_to(state.capacity.min(buf.len()));

            if state.queued.is_empty() {
                // Tell the driver that there's at least one byte ready to send.
                // NOTE: We only do this when state.queued.is_empty() as an optimization.
                self.wakeup.lock().unwrap().send(self.id);
            }

            state.capacity -= chunk.len();
            state.queued.push_back(chunk);
        }

        Ok(())
    }

    pub async fn write_all(&mut self, mut buf: &[u8]) -> Result<(), SendError> {
        while !buf.is_empty() {
            let n = self.write(buf).await?;
            buf = &buf[n..];
        }
        Ok(())
    }

    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<(), SendError> {
        let n = self.write(buf.chunk()).await?;
        buf.advance(n);
        Ok(())
    }

    pub fn finish(self) {
        let mut state = self.state.lock().unwrap();
        state.fin = true;

        if state.queued.is_empty() {
            self.wakeup.lock().unwrap().send(self.id);
        }
    }

    pub fn reset(self, code: u64) {
        let mut state = self.state.lock().unwrap();
        state.reset = Some(code);
        self.wakeup.lock().unwrap().send(self.id);
    }

    pub fn set_priority(&mut self, priority: u8) {
        self.state.lock().unwrap().priority = Some(priority);
        self.wakeup.lock().unwrap().send(self.id);
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();

        if !state.fin && state.reset.is_none() {
            state.reset = Some(0);
            self.wakeup.lock().unwrap().send(self.id);
        }
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let fut = self.write(buf);
        tokio::pin!(fut);

        Poll::Ready(
            ready!(fut.poll(cx))
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string())),
        )
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
    capacity: usize,

    // The driver wakes up the application when data is available.
    readable: Arc<Notify>,

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
            capacity: 0,
            readable: Arc::new(Notify::new()),
            fin: false,
            reset: None,
            stop: None,
            buf: BytesMut::with_capacity(64),
            buf_capacity: 64,
        }
    }

    pub fn flush(&mut self, qconn: &mut QuicheConnection) -> quiche::Result<()> {
        if let Some(_) = self.reset {
            return Ok(());
        }

        if let Some(stop) = self.stop {
            qconn.stream_shutdown(self.id.0, Shutdown::Read, stop)?;
            return Ok(());
        }

        while self.capacity > 0 {
            if self.buf.capacity() == 0 {
                // TODO get the readable size in Quiche so we can use that instead of guessing.
                self.buf_capacity = (self.buf_capacity * 2).min(32 * 1024);
                self.buf.reserve(self.buf_capacity);
            }

            // We don't actually use the buffer.len() because we immediately call split_to after reading.
            assert!(self.buf.is_empty(), "buffer should always be empty");

            // Do some unsafe to avoid zeroing the buffer.
            let buf = unsafe { std::mem::transmute(self.buf.spare_capacity_mut()) };

            match qconn.stream_recv(self.id.0, buf) {
                Ok((n, done)) => {
                    // Advance the buffer by the number of bytes read.
                    unsafe { self.buf.set_len(self.buf.len() + n) };

                    // Then split the buffer and push the front to the queue.
                    self.queued.push_back(self.buf.split_to(n).freeze());
                    self.capacity -= n;

                    if done {
                        self.fin = true;
                        break;
                    }
                }
                Err(quiche::Error::Done) => break,
                Err(quiche::Error::StreamReset(code)) => {
                    self.reset = Some(code);
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }

        // TODO notify the application

        Ok(())
    }
}

enum RecvResult {
    Success(Bytes),
    Blocked(OwnedNotified),
    Closed,
}

pub struct RecvStream {
    id: StreamId,
    state: Arc<Mutex<RecvState>>,
    wakeup: Arc<Mutex<WakeupState>>,
}

impl RecvStream {
    pub fn id(&self) -> StreamId {
        self.id
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, RecvError> {
        Ok(self.read_chunk(buf.len()).await?.map(|chunk| {
            buf.copy_from_slice(&chunk);
            chunk.len()
        }))
    }

    pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, RecvError> {
        loop {
            match self.try_read(max)? {
                RecvResult::Success(chunk) => return Ok(Some(chunk)),
                RecvResult::Blocked(notify) => notify.await,
                RecvResult::Closed => return Ok(None),
            }
        }
    }

    fn try_read(&mut self, max: usize) -> Result<RecvResult, RecvError> {
        let mut state = self.state.lock().unwrap();

        if let Some(reset) = state.reset {
            return Err(RecvError::Reset(reset));
        }

        if let Some(mut chunk) = state.queued.pop_front() {
            if chunk.len() > max {
                let remain = chunk.split_off(max);
                state.queued.push_front(remain);
            }
            return Ok(RecvResult::Success(chunk));
        }

        if state.fin {
            return Ok(RecvResult::Closed);
        }

        state.capacity = max;

        // Tell the driver that we are blocked.
        self.wakeup.lock().unwrap().recv(self.id);

        let notify = state.readable.clone().notified_owned();
        Ok(RecvResult::Blocked(notify))
    }

    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<(), RecvError> {
        match self
            .read(unsafe { std::mem::transmute(buf.chunk_mut()) })
            .await?
        {
            Some(n) => {
                unsafe { buf.advance_mut(n) };
                Ok(())
            }
            None => Err(RecvError::Closed),
        }
    }

    pub async fn read_all(&mut self, max: usize) -> Result<Bytes, RecvError> {
        let buf = BytesMut::new();
        let mut limit = buf.limit(max);
        self.read_buf(&mut limit).await?;
        Ok(limit.into_inner().freeze())
    }

    pub fn stop(self, code: u64) {
        let mut state = self.state.lock().unwrap();
        if state.reset.is_none() {
            state.stop = Some(code);
            self.wakeup.lock().unwrap().recv(self.id);
        }
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();

        if !state.fin && state.stop.is_none() {
            state.stop = Some(0);
            self.wakeup.lock().unwrap().recv(self.id);
        }
    }
}

impl AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), io::Error>> {
        let fut = self.read_buf(buf);
        tokio::pin!(fut);

        Poll::Ready(
            ready!(fut.poll(cx))
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string())),
        )
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
        todo!();
    }

    pub fn is_bi(&self) -> bool {
        !self.is_uni()
    }

    pub fn is_server(&self) -> bool {
        todo!();
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
