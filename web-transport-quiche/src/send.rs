use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, Bytes};
use futures::ready;
use tokio::io::AsyncWrite;

use crate::ez;

#[derive(thiserror::Error, Debug)]
pub enum SendError {
    #[error("connection error: {0}")]
    Connection(#[from] ez::ConnectionError),

    #[error("STOP_SENDING: {0}")]
    Stop(u32),
}

impl From<ez::SendError> for SendError {
    fn from(err: ez::SendError) -> Self {
        match err {
            ez::SendError::Stop(code) => {
                SendError::Stop(web_transport_proto::error_from_http3(code).unwrap_or(code as u32))
            }
            ez::SendError::Connection(e) => SendError::Connection(e),
        }
    }
}

pub struct SendStream {
    inner: Option<ez::SendStream>,
}

impl SendStream {
    pub(crate) fn new(inner: ez::SendStream) -> Self {
        Self { inner: Some(inner) }
    }

    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, SendError> {
        self.inner
            .as_mut()
            .unwrap()
            .write(buf)
            .await
            .map_err(Into::into)
    }

    pub async fn write_chunk(&mut self, buf: Bytes) -> Result<(), SendError> {
        self.inner
            .as_mut()
            .unwrap()
            .write_chunk(buf)
            .await
            .map_err(Into::into)
    }

    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), SendError> {
        self.inner
            .as_mut()
            .unwrap()
            .write_all(buf)
            .await
            .map_err(Into::into)
    }

    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<(), SendError> {
        self.inner
            .as_mut()
            .unwrap()
            .write_buf(buf)
            .await
            .map_err(Into::into)
    }

    pub fn finish(mut self) {
        self.inner.take().unwrap().finish()
    }

    pub fn reset(mut self, code: u32) {
        let code = web_transport_proto::error_to_http3(code);
        self.inner.take().unwrap().reset(code)
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.finish()
        }
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let fut = self.write(buf);
        tokio::pin!(fut);

        Poll::Ready(
            ready!(fut.poll(cx))
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string())),
        )
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        // Flushing happens automatically via the driver
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        // We purposely don't implement this; use finish() instead because it takes self.
        Poll::Ready(Ok(()))
    }
}
