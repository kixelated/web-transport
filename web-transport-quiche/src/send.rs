use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use tokio::io::AsyncWrite;
use tokio_quiche::quiche;

use crate::{ConnectionState, WriteError};

/// A send stream for WebTransport over Quiche.
/// Implements AsyncWrite with waker-based backpressure.
pub struct SendStream {
    /// Shared connection state.
    state: Arc<Mutex<ConnectionState>>,

    /// The QUIC stream ID.
    stream_id: u64,

    /// Whether this is a bidirectional stream (true) or unidirectional (false).
    is_bi: bool,
}

impl SendStream {
    pub(crate) fn new(state: Arc<Mutex<ConnectionState>>, stream_id: u64, is_bi: bool) -> Self {
        Self {
            state,
            stream_id,
            is_bi,
        }
    }

    /// Get the stream ID.
    pub fn id(&self) -> u64 {
        self.stream_id
    }

    /// Set the priority of the stream.
    /// Note: Quiche may not support this - we'll implement it if possible.
    pub fn set_priority(&self, _priority: i32) -> Result<(), WriteError> {
        // TODO: Check if Quiche exposes a priority API
        // For now, this is a no-op
        Ok(())
    }

    /// Stop the stream with an error code.
    pub fn stop(&mut self, error_code: u32) -> Result<(), WriteError> {
        let code = web_transport_proto::error_to_http3(error_code);
        let mut state = self.state.lock().unwrap();

        state
            .conn
            .stream_shutdown(self.stream_id, quiche::Shutdown::Write, code)
            .map_err(|e| match e {
                quiche::Error::Done => WriteError::ClosedStream,
                _ => WriteError::SessionError(e.into()),
            })?;

        Ok(())
    }

    /// Finish the stream gracefully.
    pub fn finish(&mut self) -> Result<(), WriteError> {
        let mut state = self.state.lock().unwrap();

        state
            .conn
            .stream_shutdown(self.stream_id, quiche::Shutdown::Write, 0)
            .map_err(|e| match e {
                quiche::Error::Done => WriteError::ClosedStream,
                _ => WriteError::SessionError(e.into()),
            })?;

        Ok(())
    }

    /// Write data to the stream, prepending header on first write.
    fn write_with_header(&self, state: &mut ConnectionState, buf: &[u8]) -> Result<usize, WriteError> {
        // Check if this is the first write
        let is_first_write = !state.stream_first_write.contains_key(&self.stream_id);

        if is_first_write {
            // Prepend the appropriate header
            let header = if self.is_bi {
                &state.header_bi
            } else {
                &state.header_uni
            };

            // Write header first
            let header_written = state
                .conn
                .stream_send(self.stream_id, header, false)
                .map_err(|e| match e {
                    quiche::Error::Done => WriteError::WouldBlock,
                    quiche::Error::StreamStopped(error_code) => {
                        match web_transport_proto::error_from_http3(error_code) {
                            Some(code) => WriteError::Stopped(code),
                            None => WriteError::InvalidStopped,
                        }
                    }
                    _ => WriteError::SessionError(e.into()),
                })?;

            if header_written < header.len() {
                // Partial header write - this is problematic
                // We'll need to track partial header writes in the state
                // For now, return an error
                return Err(WriteError::SessionError(
                    quiche::Error::Done.into(),
                ));
            }

            state.stream_first_write.insert(self.stream_id, true);
        }

        // Now write the actual data
        state
            .conn
            .stream_send(self.stream_id, buf, false)
            .map_err(|e| match e {
                quiche::Error::Done => WriteError::WouldBlock,
                quiche::Error::StreamStopped(error_code) => {
                    match web_transport_proto::error_from_http3(error_code) {
                        Some(code) => WriteError::Stopped(code),
                        None => WriteError::InvalidStopped,
                    }
                }
                _ => WriteError::SessionError(e.into()),
            })
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let mut state = self.state.lock().unwrap();

        match self.write_with_header(&mut state, buf) {
            Ok(written) => Poll::Ready(Ok(written)),
            Err(WriteError::WouldBlock) => {
                // Register waker and return Pending
                state.send_wakers.insert(self.stream_id, cx.waker().clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        // Quiche handles flushing at the connection level
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match self.finish() {
            Ok(()) => Poll::Ready(Ok(())),
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e))),
        }
    }
}

impl web_transport_trait::SendStream for SendStream {
    type Error = WriteError;

    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        use tokio::io::AsyncWriteExt;
        AsyncWriteExt::write_all(self, buf)
            .await
            .map_err(|e| WriteError::SessionError(crate::SessionError::Quiche(
                crate::error::QuicheError(Arc::new(quiche::Error::Done)),
            )))?;
        Ok(buf.len())
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        use tokio::io::AsyncWriteExt;
        AsyncWriteExt::write_all(self, buf)
            .await
            .map_err(|e| WriteError::SessionError(crate::SessionError::Quiche(
                crate::error::QuicheError(Arc::new(quiche::Error::Done)),
            )))
    }

    fn set_priority(&mut self, priority: i32) {
        let _ = SendStream::set_priority(self, priority);
    }

    fn reset(&mut self, error_code: u32) {
        let _ = self.stop(error_code);
    }

    async fn finish(&mut self) -> Result<(), Self::Error> {
        self.finish()
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        // TODO: Implement stream close detection
        // For now, this is a no-op
        Ok(())
    }
}
