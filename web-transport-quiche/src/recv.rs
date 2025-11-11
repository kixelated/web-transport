use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{BufMut, Bytes};
use tokio::io::{AsyncRead, ReadBuf};

use crate::{ez, SessionError};

#[derive(thiserror::Error, Debug)]
pub enum RecvError {
    #[error("session error: {0}")]
    Session(#[from] SessionError),

    #[error("reset stream: {0})")]
    Reset(u32),

    #[error("invalid reset code: {0}")]
    InvalidReset(u64),

    #[error("stream closed")]
    Closed,
}

impl From<ez::RecvError> for RecvError {
    fn from(err: ez::RecvError) -> Self {
        match err {
            ez::RecvError::Reset(code) => match web_transport_proto::error_from_http3(code) {
                Some(code) => RecvError::Reset(code),
                None => RecvError::InvalidReset(code),
            },
            ez::RecvError::Connection(e) => RecvError::Session(e.into()),
            ez::RecvError::Closed => RecvError::Closed,
        }
    }
}

pub struct RecvStream {
    inner: Option<ez::RecvStream>,
}

impl RecvStream {
    pub(crate) fn new(inner: ez::RecvStream) -> Self {
        Self { inner: Some(inner) }
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, RecvError> {
        self.inner
            .as_mut()
            .unwrap()
            .read(buf)
            .await
            .map_err(Into::into)
    }

    pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, RecvError> {
        self.inner
            .as_mut()
            .unwrap()
            .read_chunk(max)
            .await
            .map_err(Into::into)
    }

    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<(), RecvError> {
        self.inner
            .as_mut()
            .unwrap()
            .read_buf(buf)
            .await
            .map_err(Into::into)
    }

    pub async fn read_all(&mut self) -> Result<Bytes, RecvError> {
        self.inner
            .as_mut()
            .unwrap()
            .read_all()
            .await
            .map_err(Into::into)
    }

    pub fn stop(mut self, code: u32) {
        self.inner
            .take()
            .unwrap()
            .stop(web_transport_proto::error_to_http3(code));
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.stop(web_transport_proto::error_to_http3(0));
        }
    }
}

impl AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let inner = self.inner.as_mut().unwrap();
        tokio::pin!(inner);
        inner.poll_read(cx, buf)
    }
}
