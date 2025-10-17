use bytes::Buf;
use js_sys::{Reflect, Uint8Array};
use web_sys::WebTransportSendStream;

use crate::Error;
use web_streams::Writer;

/// A stream of bytes sent to the remote peer.
pub struct SendStream {
    stream: WebTransportSendStream,
    writer: Writer,
}

impl SendStream {
    pub(super) fn new(stream: WebTransportSendStream) -> Result<Self, Error> {
        let writer = Writer::new(&stream)?;
        Ok(Self { stream, writer })
    }

    /// Write *all* of the given bytes to the stream.
    pub async fn write(&mut self, buf: &[u8]) -> Result<(), Error> {
        self.writer
            .write(&Uint8Array::from(buf))
            .await
            .map_err(Into::into)
    }

    /// Writes some of the given buffer to the stream.
    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, Error> {
        let chunk = buf.chunk();
        let size = chunk.len();
        self.writer.write(&Uint8Array::from(chunk)).await?;
        buf.advance(size);
        Ok(size)
    }

    /// Send an immediate reset code, closing the stream with an error.
    pub fn reset(&mut self, reason: &str) {
        self.writer.abort(reason);
    }

    /// Mark the stream as finished.
    ///
    /// This is automatically called on Drop, but can be called manually.
    pub fn finish(&mut self) -> Result<(), Error> {
        self.writer.close();
        Ok(())
    }

    /// Set the stream's priority.
    ///
    /// Streams with **higher** values are sent first, but are not guaranteed to arrive first.
    pub fn set_priority(&mut self, priority: i32) {
        Reflect::set(&self.stream, &"sendOrder".into(), &priority.into())
            .expect("failed to set priority");
    }

    /// Block until the stream has been closed and return the error code, if any.
    pub async fn closed(&self) -> Result<Option<u8>, Error> {
        let err = match self.writer.closed().await {
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

impl Drop for SendStream {
    /// Close the stream with a FIN.
    fn drop(&mut self) {
        self.writer.close();
    }
}

impl web_transport_trait::SendStream for SendStream {
    type Error = Error;

    fn set_priority(&mut self, order: i32) {
        Self::set_priority(self, order)
    }

    fn reset(&mut self, code: u32) {
        Self::reset(self, &format!("code = {code}"));
    }

    async fn finish(&mut self) -> Result<(), Self::Error> {
        Self::finish(self)
    }

    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        Self::write(self, buf).await?;
        Ok(buf.len())
    }

    async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, Self::Error> {
        Self::write_buf(self, buf).await
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        Self::closed(self).await?;
        Ok(())
    }
}
