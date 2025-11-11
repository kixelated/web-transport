use std::{
    collections::VecDeque,
    future::poll_fn,
    io,
    pin::Pin,
    task::{ready, Context, Poll, Waker},
};
use tokio_quiche::quiche;

use bytes::{Buf, Bytes};
use tokio::io::AsyncWrite;

use tokio_quiche::quic::QuicheConnection;

use super::{DriverWakeup, Lock, StreamError, StreamId};

// "senddrop" in ascii; if you see this then call finish().await or close(code)
// decimal: 7308889627613622128
const DROP_CODE: u64 = 0x656E646464726F70;

pub(crate) struct SendState {
    id: StreamId,

    // The amount of data that is allowed to be written.
    capacity: usize,

    // Data ready to send. (capacity has been subtracted)
    queued: VecDeque<Bytes>,

    // Called by the driver when the stream is writable again.
    blocked: Option<Waker>,

    // send STREAM_FIN
    fin: bool,

    // send RESET_STREAM
    reset: Option<u64>,

    // received
    stop: Option<u64>,

    // received SET_PRIORITY
    priority: Option<u8>,
}

impl SendState {
    pub fn new(id: StreamId) -> Self {
        Self {
            id,
            capacity: 0,
            queued: VecDeque::new(),
            blocked: None,
            fin: false,
            reset: None,
            stop: None,
            priority: None,
        }
    }

    pub fn id(&self) -> StreamId {
        self.id
    }

    // Write some of the buffer to the stream, advancing the internal position.
    // Returns the number of bytes written for convenience.
    fn poll_write_buf<B: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, StreamError>> {
        println!("poll_write_buf: {:?} {:?}", self.id, buf.remaining());

        if let Some(reset) = self.reset {
            return Poll::Ready(Err(StreamError::Reset(reset)));
        }

        if let Some(stop) = self.stop {
            return Poll::Ready(Err(StreamError::Stop(stop)));
        }

        if self.fin {
            return Poll::Ready(Err(StreamError::Closed));
        }

        if self.capacity == 0 {
            self.blocked = Some(cx.waker().clone());
            println!("blocking for capacity: {:?}", self.id);
            return Poll::Pending;
        }

        let n = self.capacity.min(buf.remaining());
        println!("writing {:?} bytes: {:?} {:?}", n, self.id, buf.remaining());

        // NOTE: Avoids a copy when Buf is Bytes.
        let chunk = buf.copy_to_bytes(n);

        self.capacity -= chunk.len();
        self.queued.push_back(chunk);

        Poll::Ready(Ok(n))
    }

    pub fn poll_closed(&mut self, waker: &Waker) -> Poll<Result<(), StreamError>> {
        if let Some(reset) = self.reset {
            return Poll::Ready(Err(StreamError::Reset(reset)));
        }

        if let Some(stop) = self.stop {
            return Poll::Ready(Err(StreamError::Stop(stop)));
        }

        if self.fin && self.queued.is_empty() {
            // TODO wait until the peer has acknowledged the fin
            return Poll::Ready(Ok(()));
        }

        self.blocked = Some(waker.clone());

        Poll::Pending
    }

    pub fn flush(&mut self, qconn: &mut QuicheConnection) -> quiche::Result<Option<Waker>> {
        if let Some(reset) = self.reset {
            println!("shutting down send bi: {:?} {:?}", self.id, reset);
            qconn.stream_shutdown(self.id.into(), quiche::Shutdown::Write, reset)?;
            return Ok(self.blocked.take());
        }

        if let Some(_) = self.stop.take() {
            println!("waking blocked for stop: {:?}", self.id);
            return Ok(self.blocked.take());
        }

        if let Some(priority) = self.priority.take() {
            println!("setting priority: {:?} {:?}", self.id, priority);
            qconn.stream_priority(self.id.into(), priority, true)?;
        }

        while let Some(mut chunk) = self.queued.pop_front() {
            println!("sending chunk: {:?} {:?}", self.id, chunk.len());

            let n = match qconn.stream_send(self.id.into(), &chunk, false) {
                Ok(n) => n,
                Err(quiche::Error::Done) => 0,
                Err(quiche::Error::StreamStopped(code)) => {
                    self.stop = Some(code);
                    return Ok(self.blocked.take());
                }
                Err(e) => return Err(e.into()),
            };

            println!("sent chunk: {:?} {:?}", self.id, n);
            self.capacity -= n;
            println!("capacity after sending: {:?} {:?}", self.id, self.capacity);

            if n < chunk.len() {
                println!("queued remainder: {:?} {:?}", self.id, chunk.len() - n);

                self.queued.push_front(chunk.split_off(n));

                // Register a `stream_writable_next` callback when at least one byte is ready to send.
                qconn.stream_writable(self.id.into(), 1)?;

                break;
            }
        }

        if self.queued.is_empty() && self.fin {
            println!("sending fin: {:?}", self.id);
            qconn.stream_send(self.id.into(), &[], true)?;
            return Ok(self.blocked.take());
        }

        self.capacity = match qconn.stream_capacity(self.id.into()) {
            Ok(capacity) => capacity,
            Err(quiche::Error::StreamStopped(code)) => {
                self.stop = Some(code);
                println!("waking blocked for stop: {:?}", self.id);
                return Ok(self.blocked.take());
            }
            Err(e) => return Err(e.into()),
        };
        println!("setting capacity: {:?} {:?}", self.id, self.capacity);

        if self.capacity > 0 {
            println!("waking blocked for capacity: {:?}", self.id);
            return Ok(self.blocked.take());
        }

        // No write capacity available, so don't wake up the application.
        Ok(None)
    }
}

pub struct SendStream {
    id: StreamId,
    state: Lock<SendState>,

    // Used to wake up the driver when the stream is writable.
    wakeup: Lock<DriverWakeup>,
}

impl SendStream {
    pub(crate) fn new(id: StreamId, state: Lock<SendState>, wakeup: Lock<DriverWakeup>) -> Self {
        Self { id, state, wakeup }
    }

    pub fn id(&self) -> StreamId {
        self.id
    }

    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, StreamError> {
        let mut buf = io::Cursor::new(buf);
        poll_fn(|cx| self.poll_write_buf(cx, &mut buf)).await
    }

    // Write some of the buffer to the stream, advancing the internal position.
    // Returns the number of bytes written for convenience.
    fn poll_write_buf<B: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, StreamError>> {
        println!("poll_write_buf: {:?} {:?}", self.id, buf.remaining());

        if let Poll::Ready(res) = self.state.lock().poll_write_buf(cx, buf) {
            let waker = self.wakeup.lock().waker(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }

            return Poll::Ready(res);
        }

        Poll::Pending
    }

    /// Write all of the slice to the stream.
    pub async fn write_all(&mut self, mut buf: &[u8]) -> Result<(), StreamError> {
        while !buf.is_empty() {
            let n = self.write(buf).await?;
            buf = &buf[n..];
        }
        Ok(())
    }

    /// Write some of the buffer to the stream, advancing the internal position.
    ///
    /// Returns the number of bytes written for convenience.
    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, StreamError> {
        poll_fn(|cx| self.poll_write_buf(cx, buf)).await
    }

    /// Write the entire buffer to the stream, advancing the internal position.
    pub async fn write_buf_all<B: Buf>(&mut self, buf: &mut B) -> Result<(), StreamError> {
        while buf.has_remaining() {
            self.write_buf(buf).await?;
        }
        Ok(())
    }

    /// Mark the stream as finished.
    ///
    /// Returns an error if the stream is already closed.
    pub fn finish(&mut self) -> Result<(), StreamError> {
        {
            let mut state = self.state.lock();
            if let Some(reset) = state.reset {
                return Err(StreamError::Reset(reset));
            } else if let Some(stop) = state.stop {
                return Err(StreamError::Stop(stop));
            } else if state.fin {
                return Err(StreamError::Closed);
            }

            state.fin = true;
        }

        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }

    /// Immediately close the stream via a RESET_STREAM.
    pub fn close(&mut self, code: u64) {
        self.state.lock().reset = Some(code);

        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Returns true if the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a RESET_STREAM via [Self::close]
    /// - We received a STOP_SENDING via [RecvStream::close]
    /// - We sent a FIN via [Self::finish]
    pub fn is_closed(&self) -> bool {
        let state = self.state.lock();
        state.fin || state.reset.is_some() || state.stop.is_some()
    }

    /// Block until the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a RESET_STREAM via [Self::close]
    /// - We received a STOP_SENDING via [RecvStream::close]
    /// - We sent a FIN via [Self::finish]
    ///
    /// NOTE: This takes &mut to match Quinn and to simplify the implementation.
    /// TODO: This should block until the FIN has been acknowledged, not just sent.
    pub async fn closed(&mut self) -> Result<(), StreamError> {
        poll_fn(|cx| self.state.lock().poll_closed(cx.waker())).await
    }

    pub fn set_priority(&mut self, priority: u8) {
        self.state.lock().priority = Some(priority);

        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        let mut state = self.state.lock();

        if !state.fin && state.reset.is_none() && state.stop.is_none() {
            // Reset the stream if we're dropped without calling finish.
            state.reset = Some(DROP_CODE);
            drop(state);

            let waker = self.wakeup.lock().waker(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let mut buf = io::Cursor::new(buf);
        match ready!(self.poll_write_buf(cx, &mut buf)) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e.to_string()))),
        }
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
