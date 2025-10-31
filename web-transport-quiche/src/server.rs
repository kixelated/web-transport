use std::{
    collections::{
        hash_map::{self, OccupiedEntry, VacantEntry},
        HashMap,
    },
    future::Future,
    task::Waker,
};

use futures::stream::StreamExt;
use tokio::{net::UdpSocket, sync::mpsc, task::JoinSet};
#[cfg(not(target_os = "linux"))]
use tokio_quiche::socket::SocketCapabilities;
use tokio_quiche::{
    buf_factory::{BufFactory, PooledBuf},
    listen,
    quic::{QuicheConnection, SimpleConnectionIdGenerator},
    settings::{Hooks, QuicSettings, TlsCertificatePaths},
    socket::QuicListener,
    ApplicationOverQuic, ConnectionParams, InitialQuicConnection, QuicConnection,
    QuicConnectionStream,
};

pub use tokio_quiche::metrics::{DefaultMetrics, Metrics};
use web_transport_proto::{ConnectError, ConnectRequest, SettingsError, VarInt};

use crate::SessionError;

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

    pub async fn with_addr<A: tokio::net::ToSocketAddrs>(self, addrs: A) -> std::io::Result<Self> {
        let socket = tokio::net::UdpSocket::bind(addrs).await?;
        Ok(self.with_sockets([socket]))
    }

    pub fn with_settings(mut self, settings: QuicSettings) -> Self {
        self.settings = settings;
        self
    }

    // TODO add support for in-memory certs
    pub fn with_certs<'a>(self, tls: TlsCertificatePaths<'a>) -> std::io::Result<Server<M>> {
        let params = ConnectionParams::new_server(self.settings, tls, Hooks::default());
        let server = tokio_quiche::listen_with_capabilities(
            self.listeners,
            params,
            SimpleConnectionIdGenerator,
            self.metrics,
        )?;
        Ok(Server::new(server))
    }
}

pub struct Server<M: Metrics> {
    tasks: JoinSet<std::io::Result<()>>,
    requests: mpsc::Receiver<Session>,
}

impl<M: Metrics> Server<M> {
    fn new(sockets: Vec<QuicConnectionStream<M>>) -> Self {
        let mut tasks = JoinSet::default();
        let (tx, rx) = mpsc::channel(sockets.len());

        for socket in sockets {
            // TODO close all when one errors
            tasks.spawn(Self::run_socket(socket, tx.clone()));
        }

        Self {
            tasks,
            requests: rx,
        }
    }

    async fn run_socket(
        socket: QuicConnectionStream<M>,
        tx: mpsc::Sender<Session>,
    ) -> std::io::Result<()> {
        let mut rx = socket.into_inner();
        while let Some(initial) = rx.recv().await {
            let session = SessionDriver::new();
            let handle = initial?.start(session);
            let request = Session::new(handle);

            if tx.send(request).await.is_err() {
                return Ok(());
            }
        }

        Ok(())
    }

    // TODO get the Result and return it
    pub async fn accept(&mut self) -> Option<Session> {
        self.requests.recv().await
    }
}

pub struct Session {
    pub connection: QuicConnection,
}

impl Session {
    fn new(connection: QuicConnection) -> Self {
        Self { connection }
    }
}

struct SessionDriver {
    buf: PooledBuf,

    settings_tx_id: StreamId,
    settings_tx_buf: Vec<u8>,

    settings_rx: Option<web_transport_proto::Settings>,
    settings_rx_id: Option<StreamId>,
    settings_rx_buf: Vec<u8>,

    connect_id: Option<StreamId>,
    connect_rx: Option<web_transport_proto::ConnectRequest>,
    connect_rx_buf: Vec<u8>,
    connect_tx_buf: Vec<u8>,

    next_uni: StreamId,
    next_bi: StreamId,

    active: HashSet<StreamId>,
}

impl SessionDriver {
    fn new() -> Self {
        let mut next_uni = StreamId::SERVER_UNI;
        let next_bi = StreamId::SERVER_BI;

        let mut settings = web_transport_proto::Settings::default();
        settings.enable_webtransport(1);

        let settings_tx_id = next_uni.increment();

        let mut settings_tx_buf = Vec::new();
        settings.encode(&mut settings_tx_buf);

        Self {
            buf: BufFactory::get_max_buf(),
            settings_tx_id,
            settings_tx_buf,
            settings_rx: None,
            settings_rx_id: None,
            settings_rx_buf: Default::default(),
            connect_rx: None,
            connect_id: None,
            connect_rx_buf: Vec::new(),
            connect_tx_buf: Vec::new(),
            next_uni,
            next_bi,
            active: HashMap::new(),
        }
    }

    fn read_settings(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), SessionError> {
        let (size, end) = qconn.stream_recv(stream_id.0, &mut self.buf)?;
        if end {
            return Err(SessionError::Closed);
        }

        // TODO avoid a copy
        self.settings_rx_buf.extend_from_slice(&self.buf[..size]);

        // If the total buffered size is huge, error
        if self.settings_rx_buf.len() >= BufFactory::MAX_BUF_SIZE {
            return Err(SettingsError::InvalidSize.into());
        }

        if self.settings_rx.is_some() {
            // Ignore everything else on the stream.
            return Ok(());
        }

        let mut cursor = std::io::Cursor::new(&self.settings_rx_buf);
        web_transport_proto::Settings::decode(&mut cursor);

        let settings = match web_transport_proto::Settings::decode(&mut cursor) {
            Ok(settings) => settings,
            Err(web_transport_proto::SettingsError::UnexpectedEnd) => return Ok(()), // More data needed.
            Err(e) => return Err(e.into()),
        };

        if settings.supports_webtransport() == 0 {
            return Err(SettingsError::Unsupported.into());
        }

        self.settings_rx = Some(settings);
        self.settings_rx_buf.drain(..(cursor.position() as usize));

        Ok(())
    }

    fn read_connect(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), SessionError> {
        let (size, end) = qconn.stream_recv(stream_id.0, &mut self.buf)?;
        if end {
            return Err(SessionError::Closed);
        }

        // TODO avoid a copy
        self.connect_rx_buf.extend_from_slice(&self.buf[..size]);

        // If the total buffered size is huge, error
        if self.connect_rx_buf.len() >= BufFactory::MAX_BUF_SIZE {
            return Err(SettingsError::InvalidSize.into());
        }

        if self.connect_rx.is_some() {
            // Ignore everything else on the stream.
            // TODO parse capsules
            return Ok(());
        }

        let mut cursor = std::io::Cursor::new(&self.connect_rx_buf);
        web_transport_proto::Settings::decode(&mut cursor);

        let connect = match web_transport_proto::ConnectRequest::decode(&mut cursor) {
            Ok(connect) => connect,
            Err(web_transport_proto::ConnectError::UnexpectedEnd) => return Ok(()), // More data needed.
            Err(e) => return Err(e.into()),
        };

        self.connect_rx = Some(connect);
        self.connect_rx_buf.drain(..(cursor.position() as usize));

        // TODO expose the Request
        let resp = web_transport_proto::ConnectResponse {
            status: http::StatusCode::OK,
        };
        resp.encode(&mut self.connect_tx_buf);

        self.write_connect(qconn, stream_id)
    }

    fn read_uni(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), SessionError> {
        if stream_id == self.settings_rx_id.unwrap_or(stream_id) {
            self.settings_rx_id = Some(stream_id);
            return self.read_settings(qconn, stream_id);
        }

        // TODO remove entries on close
        // TODO don't reinsert removed entries.
        if let Some(entry) = self.streams.get_mut(&stream_id) {
            if let Some(waker) = entry.take() {
                waker.wake();
            }
            return Ok(());
        }

        if let Err(err) = self.accept_uni(qconn, stream_id) {
            log::debug!("failed to accept unidirectional stream: {err}");
        }

        Ok(())
    }

    fn accept_uni(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), SessionError> {
        let typ = web_transport_proto::StreamUni(read_varint(qconn, stream_id)?);
        if typ != web_transport_proto::StreamUni::WEBTRANSPORT {
            log::debug!("ignoring unknown unidirectional stream: {typ:?}");
            return Ok(());
        }

        let connect_id = match self.connect_id {
            Some(connect_id) => connect_id.0,
            None => return Err(SessionError::Pending),
        };

        // Read the session ID and validate it.
        let session_id = read_varint(qconn, stream_id)?;
        if session_id.into_inner() != connect_id {
            return Err(SessionError::Unknown);
        }

        self.streams.insert(stream_id, None);

        Ok(())
    }

    fn read_bi(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), SessionError> {
        if stream_id == self.connect_id.unwrap_or(stream_id) {
            self.connect_id = Some(stream_id);
            return self.read_connect(qconn, stream_id);
        }

        // TODO remove entries on close
        // TODO don't reinsert removed entries.
        if let Some(entry) = self.streams.get_mut(&stream_id) {
            if let Some(waker) = entry.take() {
                waker.wake();
            }
            return Ok(());
        }

        if let Err(err) = self.accept_bi(qconn, stream_id) {
            log::debug!("failed to accept bidirectional stream: {err}");
        }

        Ok(())
    }

    fn accept_bi(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), SessionError> {
        // TODO support partial reads I guess
        // TODO don't return an error, just skip
        let frame = web_transport_proto::Frame(read_varint(qconn, stream_id)?);
        if frame != web_transport_proto::Frame::WEBTRANSPORT {
            log::debug!("ignoring unknown bidirectional stream: {frame:?}");
            return Ok(());
        }

        let connect_id = match self.connect_id {
            Some(connect_id) => connect_id.0,
            None => return Err(SessionError::Pending),
        };

        // Read the session ID and validate it.
        let session_id = read_varint(qconn, stream_id)?;
        if session_id.into_inner() != connect_id {
            return Err(SessionError::Unknown);
        }

        self.streams.insert(stream_id, None);

        Ok(())
    }

    fn write_connect(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), SessionError> {
        let size = qconn.stream_send(stream_id.0, &self.connect_tx_buf, false)?;
        self.connect_tx_buf.drain(..size);
        Ok(())
    }

    fn write_settings(&mut self, qconn: &mut QuicheConnection) -> Result<(), SessionError> {
        let size = qconn.stream_send(self.settings_tx_id.0, &self.settings_tx_buf, false)?;
        self.settings_tx_buf.drain(..size);
        Ok(())
    }
}

// Read a varint from the stream.
// TODO add support for buffering partial reads
fn read_varint(qconn: &mut QuicheConnection, stream_id: StreamId) -> Result<VarInt, SessionError> {
    // 8 bytes is the max size of a varint
    let mut buf = [0; 8];

    // Read the first byte because it includes the length.
    let (size, _done) = qconn.stream_recv(stream_id.0, &mut buf[..1])?;
    if size != 1 {
        return Err(SessionError::Unknown);
    }

    // 0b00 = 1, 0b01 = 2, 0b10 = 4, 0b11 = 8
    let total = 1 << (buf[0] >> 6);
    let (size, _done) = qconn.stream_recv(stream_id.0, &mut buf[1..total])?;
    if size != total {
        return Err(SessionError::Unknown);
    }

    // Use a cursor to read the varint on the stack.
    let mut cursor = std::io::Cursor::new(&buf[..size]);
    let v = VarInt::decode(&mut cursor).unwrap();

    Ok(v)
}

impl ApplicationOverQuic for SessionDriver {
    fn on_conn_established(
        &mut self,
        qconn: &mut QuicheConnection,
        _handshake_info: &tokio_quiche::quic::HandshakeInfo,
    ) -> tokio_quiche::QuicResult<()> {
        self.write_settings(qconn)?;
        Ok(())
    }

    fn should_act(&self) -> bool {
        true
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.buf
    }

    fn wait_for_data(
        &mut self,
        qconn: &mut QuicheConnection,
    ) -> impl Future<Output = tokio_quiche::QuicResult<()>> + Send {
        async { qconn }
    }

    fn process_reads(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        for stream_id in qconn.stream_readable_next() {
            let stream_id = StreamId(stream_id);

            if stream_id.is_uni() {
                self.read_uni(qconn, stream_id)?;
            } else {
                self.read_bi(qconn, stream_id)?;
            }
        }

        Ok(())
    }

    fn process_writes(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        for stream_id in qconn.stream_writable_next() {
            let stream_id = StreamId(stream_id);

            if stream_id == self.settings_tx_id {
                self.write_settings(qconn);
            } else if Some(stream_id) == self.connect_id {
                self.write_connect(qconn, self.connect_id.unwrap());
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct StreamId(pub u64);

impl StreamId {
    // The first stream IDs
    pub const SERVER_UNI: StreamId = StreamId(todo!());
    pub const SERVER_BI: StreamId = StreamId(todo!());
    pub const CLIENT_UNI: StreamId = StreamId(todo!());
    pub const CLIENT_BI: StreamId = StreamId(todo!());

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
