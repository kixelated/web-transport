mod util;

use std::future::Future;

pub use crate::util::{MaybeSend, MaybeSync};
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Error trait for WebTransport operations.
///
/// Implementations must be Send + Sync + 'static for use across async boundaries.
pub trait Error: std::error::Error + MaybeSend + MaybeSync + 'static {
    // TODO: Add error code support when stabilized
    // fn code(&self) -> u32;
}

/// A WebTransport Session, able to accept/create streams and send/recv datagrams.
///
/// The session can be cloned to create multiple handles.
/// The session will be closed on drop.
pub trait Session: Clone + MaybeSend + MaybeSync + 'static {
    type SendStream: SendStream;
    type RecvStream: RecvStream;
    type Error: Error;

    /// Block until the peer creates a new unidirectional stream.
    fn accept_uni(&self)
        -> impl Future<Output = Result<Self::RecvStream, Self::Error>> + MaybeSend;

    /// Block until the peer creates a new bidirectional stream.
    fn accept_bi(
        &self,
    ) -> impl Future<Output = Result<(Self::SendStream, Self::RecvStream), Self::Error>> + MaybeSend;

    /// Open a new bidirectional stream, which may block when there are too many concurrent streams.
    fn open_bi(
        &self,
    ) -> impl Future<Output = Result<(Self::SendStream, Self::RecvStream), Self::Error>> + MaybeSend;

    /// Open a new unidirectional stream, which may block when there are too many concurrent streams.
    fn open_uni(&self) -> impl Future<Output = Result<Self::SendStream, Self::Error>> + MaybeSend;

    /// Send a datagram over the network.
    ///
    /// QUIC datagrams may be dropped for any reason:
    /// - Network congestion.
    /// - Random packet loss.
    /// - Payload is larger than `max_datagram_size()`
    /// - Peer is not receiving datagrams.
    /// - Peer has too many outstanding datagrams.
    /// - ???
    fn send_datagram(&self, payload: Bytes) -> Result<(), Self::Error>;

    /// Receive a datagram over the network.
    fn recv_datagram(&self) -> impl Future<Output = Result<Bytes, Self::Error>> + MaybeSend;

    /// The maximum size of a datagram that can be sent.
    fn max_datagram_size(&self) -> usize;

    /// Close the connection immediately with a code and reason.
    fn close(&self, code: u32, reason: &str);

    /// Block until the connection is closed.
    fn closed(&self) -> impl Future<Output = Self::Error> + MaybeSend;
}

/// An outgoing stream of bytes to the peer.
///
/// QUIC streams have flow control, which means the send rate is limited by the peer's receive window.
/// The stream will be closed with a graceful FIN when dropped.
pub trait SendStream: MaybeSend {
    type Error: Error;

    /// Write some of the buffer to the stream.
    fn write(&mut self, buf: &[u8])
        -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend;

    /// Write the given buffer to the stream, advancing the internal position.
    fn write_buf<B: Buf + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend {
        async move {
            let chunk = buf.chunk();
            let size = self.write(chunk).await?;
            buf.advance(size);
            Ok(size)
        }
    }

    /// Write the given Bytes chunk to the stream.
    ///
    /// NOTE: Bytes implements Buf, so write_buf also works.
    /// This is primarily implemented for symmetry.
    fn write_chunk(
        &mut self,
        chunk: Bytes,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        async move {
            // Just so the arg isn't mut
            let mut c = chunk;
            self.write_buf(&mut c).await?;
            Ok(())
        }
    }

    /// A helper to write all the data in the buffer.
    fn write_all(
        &mut self,
        buf: &[u8],
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        async move {
            let mut pos = 0;
            while pos < buf.len() {
                pos += self.write(&buf[pos..]).await?;
            }
            Ok(())
        }
    }

    /// A helper to write all of the data in the buffer.
    fn write_all_buf<B: Buf + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        async move {
            while buf.has_remaining() {
                self.write_buf(buf).await?;
            }
            Ok(())
        }
    }

    /// Set the stream's priority.
    ///
    /// Streams with lower values will be sent first, but are not guaranteed to arrive first.
    fn set_priority(&mut self, order: i32);

    /// Send an immediate reset code, closing the stream.
    fn reset(&mut self, code: u32);

    /// Mark the stream as finished and wait for all data to be acknowledged.
    fn finish(&mut self) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;

    /// Block until the stream is closed by either side.
    ///
    // TODO: This should be &self but that requires modifying quinn.
    fn closed(&mut self) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;
}

/// An incoming stream of bytes from the peer.
///
/// All bytes are flushed in order and the stream is flow controlled.
/// The stream will be closed with STOP_SENDING code=0 when dropped.
pub trait RecvStream: MaybeSend {
    type Error: Error;

    /// Read the next chunk of data, up to the max size.
    ///
    /// This returns a chunk of data instead of copying, which may be more efficient.
    fn read(
        &mut self,
        dst: &mut [u8],
    ) -> impl Future<Output = Result<Option<usize>, Self::Error>> + MaybeSend;

    /// Read some data into the provided buffer.
    ///
    /// The number of bytes read is returned, or None if the stream is closed.
    /// The buffer will be advanced by the number of bytes read.
    fn read_buf<B: BufMut + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<Option<usize>, Self::Error>> + MaybeSend {
        async move {
            let dst = buf.chunk_mut();
            let dst = unsafe { &mut *(dst as *mut _ as *mut [u8]) };

            let size = match self.read(dst).await? {
                Some(size) => size,
                None => return Ok(None),
            };

            unsafe { buf.advance_mut(size) };

            Ok(Some(size))
        }
    }

    /// Read the next chunk of data, up to the max size.
    ///
    /// This returns a chunk of data instead of copying, which may be more efficient.
    fn read_chunk(
        &mut self,
        max: usize,
    ) -> impl Future<Output = Result<Option<Bytes>, Self::Error>> + MaybeSend {
        async move {
            // Don't allocate too much. Write your own if you want to increase this buffer.
            let mut buf = BytesMut::with_capacity(max.min(8 * 1024));

            // TODO Test this, I think it will work?
            Ok(self.read_buf(&mut buf).await?.map(|_| buf.freeze()))
        }
    }

    /// Send a `STOP_SENDING` QUIC code.
    fn stop(&mut self, code: u32);

    /// Block until the stream has been closed and return the error code, if any.
    ///
    /// This should be &self but that requires modifying quinn.
    fn closed(&mut self) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;

    /// A helper to keep reading until the stream is closed.
    fn read_all(&mut self) -> impl Future<Output = Result<Bytes, Self::Error>> + MaybeSend {
        async move {
            let mut buf = BytesMut::new();
            self.read_all_buf(&mut buf).await?;
            Ok(buf.freeze())
        }
    }

    /// A helper to keep reading until the buffer is full.
    fn read_all_buf<B: BufMut + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend {
        async move {
            let mut size = 0;
            while buf.has_remaining_mut() {
                match self.read_buf(buf).await? {
                    Some(n) => size += n,
                    None => break,
                }
            }
            Ok(size)
        }
    }
}
