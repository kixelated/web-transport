use std::{
    io,
    pin::{pin, Pin},
    task::{ready, Context, Poll},
};

use bytes::Bytes;
use futures::Future;

use crate::{ReadError, ReadExactError, ReadToEndError};

/// A stream that can be used to recieve bytes. See [`quinn::RecvStream`].
#[derive(Debug)]
pub struct RecvStream {
    inner: quinn::RecvStream,
}

impl RecvStream {
    pub(crate) fn new(stream: quinn::RecvStream) -> Self {
        Self { inner: stream }
    }

    /// Tell the other end to stop sending data with the given error code. See [`quinn::RecvStream::stop`].
    /// This is a u32 with WebTransport since it shares the error space with HTTP/3.
    pub fn stop(&mut self, code: u32) -> Result<(), quinn::UnknownStream> {
        let code = webtransport_proto::error_to_http3(code);
        let code = quinn::VarInt::try_from(code).unwrap();
        self.inner.stop(code)
    }

    // Unfortunately, we have to wrap ReadError for a bunch of functions.

    /// Read some data into the buffer and return the amount read. See [`quinn::RecvStream::read`].
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ReadError> {
        self.inner.read(buf).await.map_err(Into::into)
    }

    /// Fill the entire buffer with data. See [`quinn::RecvStream::read_exact`].
    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError> {
        self.inner.read_exact(buf).await.map_err(Into::into)
    }

    /// Read a chunk of data from the stream. See [`quinn::RecvStream::read_chunk`].
    pub async fn read_chunk(
        &mut self,
        max_length: usize,
        ordered: bool,
    ) -> Result<Option<quinn::Chunk>, ReadError> {
        self.inner
            .read_chunk(max_length, ordered)
            .await
            .map_err(Into::into)
    }

    /// Read chunks of data from the stream. See [`quinn::RecvStream::read_chunks`].
    pub async fn read_chunks(&mut self, bufs: &mut [Bytes]) -> Result<Option<usize>, ReadError> {
        self.inner.read_chunks(bufs).await.map_err(Into::into)
    }

    /// Read until the end of the stream or the limit is hit. See [`quinn::RecvStream::read_to_end`].
    pub async fn read_to_end(&mut self, size_limit: usize) -> Result<Vec<u8>, ReadToEndError> {
        self.inner.read_to_end(size_limit).await.map_err(Into::into)
    }

    // We purposely don't expose the stream ID or 0RTT because it's not valid with WebTransport
}

impl tokio::io::AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl webtransport_generic::RecvStream for RecvStream {
    type Error = ReadError;

    fn poll_read_buf<B: bytes::BufMut>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, Self::Error>> {
        let dst = buf.chunk_mut();
        let dst = unsafe { &mut *(dst as *mut _ as *mut [u8]) };

        Poll::Ready(match ready!(pin!(RecvStream::read(self, dst)).poll(cx)) {
            Ok(Some(n)) => unsafe {
                buf.advance_mut(n);
                Ok(n)
            },
            Ok(None) => Ok(0),
            Err(err) => Err(err),
        })
    }

    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<bytes::Bytes>, Self::Error>> {
        Poll::Ready(
            match ready!(pin!(RecvStream::read_chunk(self, usize::MAX, true)).poll(cx)) {
                Ok(Some(chunk)) => Ok(Some(chunk.bytes)),
                Ok(None) => Ok(None),
                Err(e) => Err(e),
            },
        )
    }

    /// Send a `STOP_SENDING` QUIC code.
    fn close(mut self, code: u32) {
        self.stop(code).ok();
    }
}
