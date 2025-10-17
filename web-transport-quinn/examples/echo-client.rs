use std::{fs, io, path};

use anyhow::Context;
use clap::Parser;
use rustls::pki_types::CertificateDer;
use url::Url;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "https://localhost:4443")]
    url: Url,

    /// Accept the certificates at this path, encoded as PEM.
    #[arg(long)]
    tls_cert: Option<path::PathBuf>,

    /// Dangerous: Disable TLS certificate verification.
    #[arg(long, default_value = "false")]
    tls_disable_verify: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Enable info logging.
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::init_from_env(env);

    let args = Args::parse();

    let client = web_transport_quinn::ClientBuilder::new();

    let client = if args.tls_disable_verify {
        log::warn!("disabling TLS certificate verification; a MITM attack is possible");

        // Accept any certificate.
        client.dangerous().with_no_certificate_verification()?
    } else if let Some(path) = &args.tls_cert {
        // Read the PEM certificate chain
        let chain = fs::File::open(path).context("failed to open cert file")?;
        let mut chain = io::BufReader::new(chain);

        let chain: Vec<CertificateDer> = rustls_pemfile::certs(&mut chain)
            .collect::<Result<_, _>>()
            .context("failed to load certs")?;

        anyhow::ensure!(!chain.is_empty(), "could not find certificate");

        // Only accept these certificates.
        // Also available: with_server_certificate_hashes
        client.with_server_certificates(chain)?
    } else {
        // Accept any certificate that matches a system root.
        client.with_system_roots()?
    };

    log::info!("connecting to {}", args.url);

    // Connect to the given URL.
    let session = client.connect(args.url).await?;

    log::info!("connected");

    // Create a bidirectional stream.
    let (mut send, mut recv) = session.open_bi().await?;

    log::info!("created stream");

    // Send a message.
    let msg = "hello world".to_string();
    send.write_all(msg.as_bytes()).await?;
    log::info!("sent: {msg}");

    // Shut down the send stream.
    send.finish()?;

    // Read back the message.
    let msg = recv.read_to_end(1024).await?;
    log::info!("recv: {}", String::from_utf8_lossy(&msg));

    Ok(())
}
