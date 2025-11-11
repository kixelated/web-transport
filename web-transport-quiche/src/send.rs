use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Buf;
use tokio::io::AsyncWrite;

use crate::{ez, StreamError};

// "send" in ascii; if you see this then call finish().await or close(code)
// hex: 0x73656E64, or 0x52E51B4DCE20 as an HTTP error code
// decimal: 1685221232, or 91143959072288 as an HTTP error code
const DROP_CODE: u64 = web_transport_proto::error_to_http3(0x73656E64);

pub struct SendStream {
    inner: ez::SendStream,
}

impl SendStream {
    pub(crate) fn new(inner: ez::SendStream) -> Self {
        Self { inner }
    }

    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, StreamError> {
        self.inner.write(buf).await.map_err(Into::into)
    }

    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, StreamError> {
        self.inner.write_buf(buf).await.map_err(Into::into)
    }

    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), StreamError> {
        self.inner.write_all(buf).await.map_err(Into::into)
    }

    pub async fn write_buf_all<B: Buf>(&mut self, buf: &mut B) -> Result<(), StreamError> {
        self.inner.write_buf_all(buf).await.map_err(Into::into)
    }

    pub fn finish(&mut self) -> Result<(), StreamError> {
        self.inner.finish().map_err(Into::into)
    }

    pub fn set_priority(&mut self, order: u8) {
        self.inner.set_priority(order)
    }

    pub fn close(&mut self, code: u32) {
        let code = web_transport_proto::error_to_http3(code);
        self.inner.close(code)
    }

    pub async fn closed(&mut self) -> Result<(), StreamError> {
        self.inner.closed().await.map_err(Into::into)
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        // Reset the stream if we dropped without calling `close` or `finish`
        if !self.inner.is_closed() {
            log::warn!("stream dropped without `close` or `finish`");
            self.inner.close(DROP_CODE)
        }
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let inner = std::pin::pin!(&mut self.inner);
        inner.poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let inner = std::pin::pin!(&mut self.inner);
        inner.poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        let inner = std::pin::pin!(&mut self.inner);
        inner.poll_shutdown(cx)
    }
}

impl web_transport_trait::SendStream for SendStream {
    type Error = StreamError;

    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.write(buf).await
    }

    fn set_priority(&mut self, order: u8) {
        self.set_priority(order)
    }

    fn close(&mut self, code: u32) {
        self.close(code)
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        self.finish()
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        self.closed().await
    }
}
