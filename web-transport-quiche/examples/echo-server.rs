use std::path;

use anyhow::Context;

use bytes::Bytes;
use clap::Parser;
use tokio_quiche::settings::{CertificateKind, TlsCertificatePaths};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "[::]:4443")]
    addr: std::net::SocketAddr,

    /// Use the certificates at this path, encoded as PEM.
    #[arg(long)]
    tls_cert: path::PathBuf,

    /// Use the private key at this path, encoded as PEM.
    #[arg(long)]
    tls_key: path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Enable info logging.
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::init_from_env(env);

    let args = Args::parse();

    let tls = TlsCertificatePaths {
        cert: args
            .tls_cert
            .to_str()
            .context("failed to convert path to str")?,
        private_key: args
            .tls_key
            .to_str()
            .context("failed to convert path to str")?,
        kind: CertificateKind::X509,
    };

    let server = web_transport_quiche::ez::ServerBuilder::default()
        .with_addr(args.addr)?
        .with_certs(tls)?;

    let mut server = web_transport_quiche::Server::new(server);

    log::info!("listening on {}", args.addr);

    // Accept new connections.
    while let Some(conn) = server.accept().await {
        tokio::spawn(async move {
            match run_conn(conn).await {
                Ok(()) => log::info!("connection closed"),
                Err(err) => log::error!("connection closed: {err}"),
            }
        });
    }

    Ok(())
}

async fn run_conn(request: web_transport_quiche::Request) -> anyhow::Result<()> {
    log::info!("received WebTransport request: {}", request.url());

    // Accept the session.
    let session = request.ok().await.context("failed to accept session")?;
    log::info!("accepted session");

    loop {
        let (mut send, mut recv) = session.accept_bi().await?;

        // Wait for a bidirectional stream or datagram (TODO).
        log::info!("accepted stream");

        // Read the message and echo it back.
        let mut msg: Bytes = recv.read_all(1024).await?;
        log::info!("recv: {}", String::from_utf8_lossy(&msg));

        send.write_buf(&mut msg).await?;
        log::info!("send: {}", String::from_utf8_lossy(&msg));

        log::info!("echo successful!");
    }
}
