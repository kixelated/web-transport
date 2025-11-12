use std::io;
use std::sync::Arc;
use tokio_quiche::settings::{Hooks, TlsCertificatePaths};

use super::{
    CertificateKind, CertificatePath, Connection, ConnectionClosed, DefaultMetrics, Driver,
    DriverWakeup, Lock, Metrics, Settings,
};

pub struct ClientBuilder<M: Metrics = DefaultMetrics> {
    settings: Settings,
    socket: Option<tokio::net::UdpSocket>,
    tls: Option<(String, String, CertificateKind)>,
    metrics: M,
}

impl Default for ClientBuilder<DefaultMetrics> {
    fn default() -> Self {
        Self::with_metrics(DefaultMetrics::default())
    }
}

impl<M: Metrics> ClientBuilder<M> {
    pub fn with_metrics(m: M) -> Self {
        let mut settings = Settings::default();
        settings.verify_peer = true;

        Self {
            settings,
            metrics: m,
            socket: None,
            tls: None,
        }
    }

    pub fn with_socket(self, socket: std::net::UdpSocket) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let socket = tokio::net::UdpSocket::from_std(socket)?;

        /*
        // TODO Modify quiche to add other platform support.
        #[cfg(target_os = "linux")]
        let capabilities = SocketCapabilities::apply_all_and_get_compatibility(&socket);
        #[cfg(not(target_os = "linux"))]
        let capabilities = SocketCapabilities::default();
        */

        Ok(Self {
            socket: Some(socket),
            settings: self.settings,
            metrics: self.metrics,
            tls: self.tls,
        })
    }

    pub fn with_bind<A: std::net::ToSocketAddrs>(self, addrs: A) -> io::Result<Self> {
        // We use std to avoid async
        let socket = std::net::UdpSocket::bind(addrs)?;
        self.with_socket(socket)
    }

    /// Use the provided [QuicSettings] instead of the defaults.
    ///
    /// WARNING: [QuicSettings::verify_peer] is set to false by default.
    /// This will completely bypass certificate verification and is generally not recommended.
    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    // TODO add support for in-memory certs
    // TODO add support for multiple certs
    pub fn with_cert(self, tls: CertificatePath<'_>) -> io::Result<Self> {
        Ok(Self {
            tls: Some((tls.cert.to_owned(), tls.private_key.to_owned(), tls.kind)),
            settings: self.settings,
            metrics: self.metrics,
            socket: self.socket,
        })
    }

    /// Connect to the server with the given host and port.
    ///
    /// This takes ownership because [tokio_quiche] doesn't support reusing the same socket for clients.
    pub async fn connect(mut self, host: &str, port: u16) -> io::Result<Connection> {
        if self.socket.is_none() {
            self = self.with_bind("[::]:0")?;
        }

        let socket = self.socket.take().unwrap();

        let mut remotes = match tokio::net::lookup_host((host, port)).await {
            Ok(remotes) => remotes,
            Err(err) => {
                return Err(io::Error::new(
                    io::ErrorKind::HostUnreachable,
                    err.to_string(),
                ));
            }
        };

        // Return the first entry.
        let remote = match remotes.next() {
            Some(remote) => remote,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::HostUnreachable,
                    "no addresses found for host",
                ))
            }
        };

        socket.connect(remote).await?;

        // Connect to the server using the addr we just resolved.
        let socket = tokio_quiche::socket::Socket::<
            Arc<tokio::net::UdpSocket>,
            Arc<tokio::net::UdpSocket>,
        >::from_udp(socket)?;

        let tls = self
            .tls
            .as_ref()
            .map(|(cert, private_key, kind)| TlsCertificatePaths {
                cert: cert.as_str(),
                private_key: private_key.as_str(),
                kind: kind.clone(),
            });

        if !self.settings.verify_peer {
            tracing::warn!("TLS certificate verification is disabled, a MITM attack is possible");
        }

        let params =
            tokio_quiche::ConnectionParams::new_client(self.settings, tls, Hooks::default());

        let accept_bi = flume::unbounded();
        let accept_uni = flume::unbounded();

        let open_bi = flume::bounded(1);
        let open_uni = flume::bounded(1);

        let send_wakeup = Lock::new(DriverWakeup::default(), "send_wakeup");
        let recv_wakeup = Lock::new(DriverWakeup::default(), "recv_wakeup");

        let closed_local = ConnectionClosed::default();
        let closed_remote = ConnectionClosed::default();

        let driver = Driver::new(
            send_wakeup.clone(),
            recv_wakeup.clone(),
            accept_bi.0,
            accept_uni.0,
            open_bi.1,
            open_uni.1,
            closed_local.clone(),
            closed_remote.clone(),
        );

        let conn = tokio_quiche::quic::connect_with_config(socket, Some(host), &params, driver)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let conn = Connection::new(
            conn,
            false,
            accept_bi.1,
            accept_uni.1,
            open_bi.0,
            open_uni.0,
            send_wakeup,
            recv_wakeup,
            closed_local,
            closed_remote,
        );

        Ok(conn)
    }
}
