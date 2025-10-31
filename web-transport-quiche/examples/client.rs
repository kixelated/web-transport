// Example client for web-transport-quiche
// NOTE: This is a skeleton example. The implementation needs to be completed first.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;
use web_transport_quiche::Client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    // Parse command line arguments
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://localhost:4433".to_string());
    let url = Url::parse(&url)?;

    println!("Connecting to {}", url);

    // Create a client (currently returns error - needs implementation)
    let client = Client::new();

    // Connect to the server
    let session = match client.connect(url).await {
        Ok(session) => {
            println!("Connected successfully!");
            session
        }
        Err(e) => {
            eprintln!("Failed to connect: {}", e);
            eprintln!("\nNOTE: This example requires the full implementation to be completed.");
            eprintln!("See README.md for what needs to be implemented.");
            return Ok(());
        }
    };

    // Open a bidirectional stream
    let (mut send, mut recv) = session.open_bi().await?;
    println!("Opened bidirectional stream");

    // Send a message
    let message = b"Hello from Quiche WebTransport!";
    send.write_all(message).await?;
    send.finish()?;
    println!("Sent: {:?}", String::from_utf8_lossy(message));

    // Receive response
    let mut buf = vec![0u8; 1024];
    let n = recv.read(&mut buf).await?;
    println!("Received: {:?}", String::from_utf8_lossy(&buf[..n]));

    // Close the session
    session.close(0, "Done");
    println!("Session closed");

    Ok(())
}
