use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Buf;
use tokio::io::AsyncWrite;

use crate::{ez, SessionError};

#[derive(thiserror::Error, Debug)]
pub enum SendError {
    #[error("session error: {0}")]
    Session(#[from] SessionError),

    #[error("stop sending: {0}")]
    Stop(u32),

    #[error("invalid stop code: {0}")]
    InvalidStop(u64),
}

impl From<ez::SendError> for SendError {
    fn from(err: ez::SendError) -> Self {
        match err {
            ez::SendError::Stop(code) => match web_transport_proto::error_from_http3(code) {
                Some(code) => SendError::Stop(code),
                None => SendError::InvalidStop(code),
            },
            ez::SendError::Connection(e) => SendError::Session(e.into()),
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

    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, SendError> {
        self.inner
            .as_mut()
            .unwrap()
            .write_buf(buf)
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

    pub async fn write_buf_all<B: Buf>(&mut self, buf: &mut B) -> Result<(), SendError> {
        self.inner
            .as_mut()
            .unwrap()
            .write_buf_all(buf)
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
        let inner = self.inner.as_mut().unwrap();
        tokio::pin!(inner);
        inner.poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let inner = self.inner.as_mut().unwrap();
        tokio::pin!(inner);
        inner.poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        let inner = self.inner.as_mut().unwrap();
        tokio::pin!(inner);
        inner.poll_shutdown(cx)
    }
}
