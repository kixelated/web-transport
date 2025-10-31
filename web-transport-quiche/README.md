# web-transport-quiche

WebTransport implementation using the Quiche QUIC library.

## Status: 🚧 Work in Progress

This is a partial implementation that demonstrates the architecture for WebTransport over Quiche. The core async I/O infrastructure is complete, but protocol-level integration (HTTP/3 handshake, stream acceptance) needs to be finished.

## What's Implemented

### ✅ Core Infrastructure
- **ConnectionState** - Shared state with waker maps for async backpressure
- **SendStream** - `AsyncWrite` with zero-buffer, waker-based I/O
- **RecvStream** - `AsyncRead` with zero-buffer, waker-based I/O
- **Session** - Main WebTransport API (accept/open streams, datagrams)
- **WebTransportDriver** - `ApplicationOverQuic` implementation
- **Error types** - Complete error hierarchy

### ✅ Key Features
- **Zero-copy I/O** - No data buffering, direct Quiche calls
- **Waker-based backpressure** - Efficient async without buffering
- **Send-safe** - All types are `Send + Sync` where needed
- **Trait compatibility** - Implements `web_transport_trait`
- **Similar API to web-transport-quinn** - Easy migration

## Architecture

### Stream I/O Flow

1. **Application writes to SendStream** → `poll_write()`
2. **SendStream calls** `conn.stream_send()` directly
3. **If blocked** (`Error::Done`) → register waker, return `Poll::Pending`
4. **Driver's `process_writes()`** → calls `stream_writable_next()`
5. **Driver wakes** the registered waker
6. **Application's write completes**

The same pattern applies for reading via RecvStream.

### Key Design Decisions

- **No data buffering** - All I/O is zero-copy through Quiche
- **Wakers stored in hashmaps** - O(1) lookup by stream ID
- **No locks across awaits** - Uses take/put pattern for channel receivers
- **Headers prepended on first write** - Automatic session ID tagging

## What Needs to Be Completed

### 1. HTTP/3 Handshake in Driver
The `ApplicationOverQuic::on_conn_established()` method needs to:
- Exchange HTTP/3 SETTINGS frames (using `web_transport_proto::Settings`)
- Send/receive CONNECT request (using `web_transport_proto::ConnectRequest`)
- Extract session ID from CONNECT stream ID

### 2. Stream Acceptance
The `ApplicationOverQuic::process_reads()` method needs to:
- Accept new streams from Quiche
- Decode WebTransport headers (session ID, stream type)
- Send accepted streams to Session via channels

### 3. Stream ID Allocation
The `Session::open_bi()` and `open_uni()` methods need proper stream ID allocation from Quiche.

### 4. Client Integration
The `Client::connect()` method needs to:
- Parse URL and resolve DNS
- Create Quiche config with proper ALPN
- Call `tokio_quiche::connect()` with WebTransportDriver
- Return Session after handshake completes

### 5. Server Implementation
Create `Server`, `ServerBuilder`, and `Request` types following the Quinn pattern.

## Example Usage (Once Complete)

```rust
use web_transport_quiche::{ClientBuilder, Session};
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a client
    let client = ClientBuilder::new()
        .with_system_roots()?;

    // Connect to a WebTransport server
    let url = Url::parse("https://localhost:4433")?;
    let session = client.connect(url).await?;

    // Open a bidirectional stream
    let (mut send, mut recv) = session.open_bi().await?;

    // Write data
    use tokio::io::AsyncWriteExt;
    send.write_all(b"Hello, WebTransport!").await?;

    // Read data
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 1024];
    let n = recv.read(&mut buf).await?;
    println!("Received: {:?}", &buf[..n]);

    Ok(())
}
```

## Comparison with web-transport-quinn

| Feature | Quinn | Quiche |
|---------|-------|--------|
| Async model | Fully async | Waker-based (poll + async) |
| Buffering | Zero-copy | Zero-copy |
| Stream API | `AsyncRead`/`AsyncWrite` | `AsyncRead`/`AsyncWrite` |
| QUIC library | Quinn | Quiche |
| TLS | rustls | BoringSSL |
| Stream creation | Object-based | ID-based (wrapped) |

## Testing

To test the implementation once complete:
```bash
cargo test --package web-transport-quiche
```

## Contributing

This implementation was created as a foundation. To complete it:
1. Implement HTTP/3 handshake logic in `driver.rs`
2. Add stream acceptance with header decoding
3. Complete Client/Server integration
4. Add examples and tests

## License

MIT OR Apache-2.0
