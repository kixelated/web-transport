use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, ReadBuf};
use tokio_quiche::quiche;

use crate::{ConnectionState, ReadError};

/// A receive stream for WebTransport over Quiche.
/// Implements AsyncRead with waker-based backpressure.
pub struct RecvStream {
    /// Shared connection state.
    state: Arc<Mutex<ConnectionState>>,

    /// The QUIC stream ID.
    stream_id: u64,
}

impl RecvStream {
    pub(crate) fn new(state: Arc<Mutex<ConnectionState>>, stream_id: u64) -> Self {
        Self { state, stream_id }
    }

    /// Get the stream ID.
    pub fn id(&self) -> u64 {
        self.stream_id
    }

    /// Stop the stream with an error code.
    pub fn stop(&mut self, error_code: u32) -> Result<(), ReadError> {
        let code = web_transport_proto::error_to_http3(error_code);
        let mut state = self.state.lock().unwrap();

        state
            .conn
            .stream_shutdown(self.stream_id, quiche::Shutdown::Read, code)
            .map_err(|e| match e {
                quiche::Error::Done => ReadError::ClosedStream,
                _ => ReadError::SessionError(e.into()),
            })?;

        Ok(())
    }
}

impl AsyncRead for RecvStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = self.state.lock().unwrap();

        match state
            .conn
            .stream_recv(self.stream_id, buf.initialize_unfilled())
        {
            Ok((read, _fin)) => {
                buf.advance(read);
                Poll::Ready(Ok(()))
            }
            Err(quiche::Error::Done) => {
                // Register waker and return Pending
                state.recv_wakers.insert(self.stream_id, cx.waker().clone());
                Poll::Pending
            }
            Err(quiche::Error::StreamReset(error_code)) => {
                let err = match web_transport_proto::error_from_http3(error_code) {
                    Some(code) => ReadError::Reset(code),
                    None => ReadError::InvalidReset,
                };
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, err)))
            }
            Err(e) => {
                let err = ReadError::SessionError(e.into());
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, err)))
            }
        }
    }
}

impl web_transport_trait::RecvStream for RecvStream {
    type Error = ReadError;

    async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        use tokio::io::AsyncReadExt;
        match AsyncReadExt::read(self, buf).await {
            Ok(0) => Ok(None), // EOF
            Ok(n) => Ok(Some(n)),
            Err(_e) => Err(ReadError::SessionError(crate::SessionError::Quiche(
                crate::error::QuicheError(Arc::new(quiche::Error::Done)),
            ))),
        }
    }

    fn stop(&mut self, error_code: u32) {
        let _ = RecvStream::stop(self, error_code);
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        // TODO: Implement stream close detection
        // For now, this is a no-op
        Ok(())
    }
}
