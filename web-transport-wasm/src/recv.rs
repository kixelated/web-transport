use bytes::{BufMut, Bytes, BytesMut};
use js_sys::Uint8Array;
use std::cmp;
use web_sys::WebTransportReceiveStream;

use crate::Error;
use web_streams::Reader;

/// A stream of bytes received from the remote peer.
///
/// This can be closed by either side with an error code, or closed by the remote with a FIN.
pub struct RecvStream {
    reader: Reader<Uint8Array>,
    buffer: BytesMut,
}

impl RecvStream {
    pub(super) fn new(stream: WebTransportReceiveStream) -> Result<Self, Error> {
        let reader = Reader::new(&stream)?;

        Ok(Self {
            reader,
            buffer: BytesMut::new(),
        })
    }

    /// Read the next chunk of data with the provided maximum size.
    ///
    /// This returns a chunk of data instead of copying, which may be more efficient.
    pub async fn read(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
        if !self.buffer.is_empty() {
            let size = cmp::min(max, self.buffer.len());
            let data = self.buffer.split_to(size).freeze();
            return Ok(Some(data));
        }

        let mut data: Bytes = match self.reader.read().await? {
            // TODO can we avoid making a copy here?
            Some(data) => data.to_vec().into(),
            None => return Ok(None),
        };

        if data.len() > max {
            // The chunk is too big; add the tail to the buffer for next read.
            self.buffer.extend_from_slice(&data.split_off(max));
        }

        Ok(Some(data))
    }

    /// Read some data into the provided buffer.
    ///
    /// Returns the (non-zero) number of bytes read, or None if the stream is closed.
    /// Advances the buffer by the number of bytes read.
    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<Option<usize>, Error> {
        let chunk = match self.read(buf.remaining_mut()).await? {
            Some(chunk) => chunk,
            None => return Ok(None),
        };

        let size = chunk.len();
        buf.put(chunk);

        Ok(Some(size))
    }

    /// Abort reading from the stream with the given reason.
    pub fn stop(&mut self, reason: &str) {
        self.reader.abort(reason);
    }

    /// Block until the stream has been closed and return the error code, if any.
    pub async fn closed(&self) -> Result<Option<u8>, Error> {
        let err = match self.reader.closed().await {
            Ok(()) => return Ok(None),
            Err(err) => Error::from(err),
        };

        // If it's a WebTransportError, we can extract the error code.
        if let Error::Stream(err) = &err {
            if let Some(code) = err.stream_error_code() {
                return Ok(Some(code));
            }
        }

        Err(err)
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        self.reader.abort("dropped");
    }
}

#[cfg(feature = "tokio")]
mod tokio_impl {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, ReadBuf};
    use Poll::Ready;
    impl AsyncRead for RecvStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            // Serve from buffer if available
            if !self.buffer.is_empty() {
                let to_copy = std::cmp::min(buf.remaining(), self.buffer.len());
                let data = self.buffer.split_to(to_copy);
                buf.put_slice(&data);
                return Ready(Ok(()));
            }

            //we can just skip this extra buffer as AsyncRead of webstreams already handles this
            Pin::new(&mut self.reader).poll_read(cx, buf)
        }
    }
}
