use std::{
    error::Error,
    fmt,
    future::Future,
    ops,
    pin::{pin, Pin},
    task::{ready, Context, Poll},
};

use quinn::VarInt;
use tokio::io::AsyncWrite;

pub struct SendStream(quinn::SendStream);

impl ops::Deref for SendStream {
    type Target = quinn::SendStream;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ops::DerefMut for SendStream {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<quinn::SendStream> for SendStream {
    fn from(stream: quinn::SendStream) -> Self {
        SendStream(stream)
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        quinn::SendStream::poll_write(Pin::new(&mut self.0), cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        quinn::SendStream::poll_flush(Pin::new(&mut self.0), cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        quinn::SendStream::poll_shutdown(Pin::new(&mut self.0), cx)
    }
}

impl webtransport_generic::SendStream for SendStream {
    type Error = WriteError;

    fn priority(&mut self, order: i32) {
        quinn::SendStream::set_priority(self, order).ok();
    }

    fn close(mut self, code: u32) {
        quinn::SendStream::reset(&mut self, VarInt::from_u32(code)).ok();
    }

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
        pin!(quinn::SendStream::write(self, buf))
            .poll(cx)
            .map(|res| res.map_err(Into::into))
    }

    fn poll_write_buf<B: bytes::Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, Self::Error>> {
        Poll::Ready(match ready!(self.poll_write(cx, buf.chunk())) {
            Ok(n) => {
                buf.advance(n);
                Ok(n)
            }
            Err(e) => Err(e),
        })
    }
}

#[derive(Clone)]
pub struct WriteError(quinn::WriteError);

impl From<quinn::WriteError> for WriteError {
    fn from(error: quinn::WriteError) -> Self {
        WriteError(error)
    }
}

impl ops::Deref for WriteError {
    type Target = quinn::WriteError;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ops::DerefMut for WriteError {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Error for WriteError {}

impl fmt::Debug for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl webtransport_generic::ErrorCode for WriteError {
    fn code(&self) -> Option<u32> {
        match &self.0 {
            quinn::WriteError::Stopped(code) => TryInto::<u32>::try_into(code.into_inner()).ok(),
            _ => None,
        }
    }
}
