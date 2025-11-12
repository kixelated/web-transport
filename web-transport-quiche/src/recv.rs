use std::{
    pin::{pin, Pin},
    task::{Context, Poll},
};

use bytes::{BufMut, Bytes};
use tokio::io::{AsyncRead, ReadBuf};

use crate::{ez, StreamError};

// "recv" in ascii; if you see this then read everything or close(code)
// hex: 0x44454356, or 0x52E4EA9B7F80 as an HTTP error code
// decimal: 1146556178, or 91143142080384 as an HTTP error code
const DROP_CODE: u64 = web_transport_proto::error_to_http3(0x44454356);

pub struct RecvStream {
    inner: ez::RecvStream,
}

impl RecvStream {
    pub(crate) fn new(inner: ez::RecvStream) -> Self {
        Self { inner }
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, StreamError> {
        self.inner.read(buf).await.map_err(Into::into)
    }

    pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, StreamError> {
        self.inner.read_chunk(max).await.map_err(Into::into)
    }

    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<Option<usize>, StreamError> {
        self.inner.read_buf(buf).await.map_err(Into::into)
    }

    pub async fn read_all(&mut self) -> Result<Bytes, StreamError> {
        self.inner.read_all().await.map_err(Into::into)
    }

    pub fn close(&mut self, code: u32) {
        self.inner.close(web_transport_proto::error_to_http3(code));
    }

    pub async fn closed(&mut self) -> Result<(), StreamError> {
        self.inner.closed().await.map_err(Into::into)
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        if !self.inner.is_closed() {
            tracing::warn!("stream dropped without `close` or `finish`");
            self.inner.close(DROP_CODE)
        }
    }
}

impl AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let pinned = pin!(&mut self.inner);
        pinned.poll_read(cx, buf)
    }
}

impl web_transport_trait::RecvStream for RecvStream {
    type Error = StreamError;

    async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        self.read(dst).await
    }

    async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Self::Error> {
        // More efficient than the default read_chunk implementation.
        self.read_chunk(max).await
    }

    fn close(&mut self, code: u32) {
        self.close(code);
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        self.closed().await
    }
}
