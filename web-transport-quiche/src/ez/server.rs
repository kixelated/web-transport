use std::{io, marker::PhantomData};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
#[cfg(not(target_os = "linux"))]
use tokio_quiche::socket::SocketCapabilities;
use tokio_quiche::{
    quic::SimpleConnectionIdGenerator,
    settings::{Hooks, TlsCertificatePaths},
    socket::QuicListener,
};

use super::{
    Connection, ConnectionClosed, DefaultMetrics, Driver, DriverWakeup, Lock, Metrics, Settings,
};

/// Used with [ServerBuilder] to require specific parameters.
#[derive(Default)]
pub struct ServerInit {}

/// Used with [ServerBuilder] to require at least one listener.
#[derive(Default)]
pub struct ServerWithListener {
    listeners: Vec<QuicListener>,
}

pub struct ServerBuilder<M: Metrics = DefaultMetrics, S = ServerInit> {
    settings: Settings,
    metrics: M,
    state: S,
}

impl Default for ServerBuilder<DefaultMetrics> {
    fn default() -> Self {
        Self::new(DefaultMetrics::default())
    }
}

impl<M: Metrics> ServerBuilder<M, ServerInit> {
    pub fn new(m: M) -> Self {
        Self {
            settings: Settings::default(),
            metrics: m,
            state: ServerInit {},
        }
    }

    fn next(self) -> ServerBuilder<M, ServerWithListener> {
        ServerBuilder {
            settings: self.settings,
            metrics: self.metrics,
            state: ServerWithListener { listeners: vec![] },
        }
    }

    pub fn with_listener(self, listener: QuicListener) -> ServerBuilder<M, ServerWithListener> {
        self.next().with_listener(listener)
    }

    pub fn with_socket(
        self,
        socket: std::net::UdpSocket,
    ) -> io::Result<ServerBuilder<M, ServerWithListener>> {
        self.next().with_socket(socket)
    }

    pub fn with_bind<A: std::net::ToSocketAddrs>(
        self,
        addrs: A,
    ) -> io::Result<ServerBuilder<M, ServerWithListener>> {
        self.next().with_bind(addrs)
    }

    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }
}

impl<M: Metrics> ServerBuilder<M, ServerWithListener> {
    pub fn with_listener(mut self, listener: QuicListener) -> Self {
        self.state.listeners.push(listener);
        self
    }

    pub fn with_socket(self, socket: std::net::UdpSocket) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let socket = tokio::net::UdpSocket::from_std(socket)?;

        // TODO Modify quiche to add other platform support.
        #[cfg(target_os = "linux")]
        let capabilities = SocketCapabilities::apply_all_and_get_compatibility(&socket);
        #[cfg(not(target_os = "linux"))]
        let capabilities = SocketCapabilities::default();

        let listener = QuicListener {
            socket,
            socket_cookie: self.state.listeners.len() as _,
            capabilities,
        };

        Ok(self.with_listener(listener))
    }

    pub fn with_bind<A: std::net::ToSocketAddrs>(self, addrs: A) -> io::Result<Self> {
        // We use std to avoid async
        let socket = std::net::UdpSocket::bind(addrs)?;
        self.with_socket(socket)
    }

    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    // TODO add support for in-memory certs
    // TODO add support for multiple certs
    pub fn with_cert<'a>(self, tls: TlsCertificatePaths<'a>) -> io::Result<Server<M>> {
        let params =
            tokio_quiche::ConnectionParams::new_server(self.settings, tls, Hooks::default());
        let server = tokio_quiche::listen_with_capabilities(
            self.state.listeners,
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

            let accept_bi = flume::unbounded();
            let accept_uni = flume::unbounded();

            let open_bi = flume::bounded(1);
            let open_uni = flume::bounded(1);

            let send_wakeup = Lock::new(DriverWakeup::default(), "send_wakeup");
            let recv_wakeup = Lock::new(DriverWakeup::default(), "recv_wakeup");

            let closed_local = ConnectionClosed::default();
            let closed_remote = ConnectionClosed::default();

            let session = Driver::new(
                send_wakeup.clone(),
                recv_wakeup.clone(),
                accept_bi.0,
                accept_uni.0,
                open_bi.1,
                open_uni.1,
                closed_local.clone(),
                closed_remote.clone(),
            );

            let inner = initial.start(session);
            let connection = Connection::new(
                inner,
                true,
                accept_bi.1,
                accept_uni.1,
                open_bi.0,
                open_uni.0,
                send_wakeup,
                recv_wakeup,
                closed_local,
                closed_remote,
            );

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
