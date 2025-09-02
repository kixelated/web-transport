use tokio::net::TcpListener;
use web_transport_generic::{RecvStream, SendStream, Session as _};
use web_transport_socket::Session;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:3000";
    let listener = TcpListener::bind(addr).await?;
    println!("WebSocket server listening on ws://{addr}");

    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(async move {
            println!("New connection from: {addr}");
            if let Err(e) = run(stream).await {
                println!("Connection error: {e}");
            } else {
                println!("Connection ended");
            }
        });
    }

    Ok(())
}

async fn run(stream: tokio::net::TcpStream) -> anyhow::Result<()> {
    let session = Session::accept(stream).await?;
    println!("WebSocket connection established");

    loop {
        tokio::select! {
            Ok(mut uni) = session.accept_uni() => {
                println!("Accepted unidirectional stream");

                // Make a unidirectional stream to echo back the received messages
                let mut echo = session.open_uni().await?;

                println!("Created unidirectional stream to echo back");

                // NOTE: You should spawn a task to read in parallel
                while let Some(data) = uni.read().await? {
                    println!(
                        "Received {} bytes on unidirectional stream: {}",
                        data.len(),
                        String::from_utf8_lossy(&data)
                    );

                    println!("Echoing back {} bytes on unidirectional stream: {}", data.len(), String::from_utf8_lossy(&data));
                    echo.write_all(&data).await?;
                }

                echo.finish().await?; // optional, wait for an ack

                println!("Unidirectional stream closed");
            }
            Ok((mut send, mut recv)) = session.accept_bi() => {
                println!("Accepted bidirectional stream");

                // NOTE: You should spawn a task to read in parallel
                while let Some(data) = recv.read().await? {
                    println!("Received {} bytes on bidirectional stream", data.len());
                    send.write_all(&data).await?;
                    println!("Echoing back {} bytes on bidirectional stream: {}", data.len(), String::from_utf8_lossy(&data));
                }

                send.finish().await?; // optional, wait for an ack

                println!("Bidirectional stream closed");
            }
            err = session.closed() => return Err(err.into()),
        }
    }
}
