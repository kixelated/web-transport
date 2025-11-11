use futures::ready;
use std::{
    collections::VecDeque,
    future::poll_fn,
    io,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use tokio_quiche::quiche;

use bytes::{BufMut, Bytes, BytesMut};
use tokio::io::{AsyncRead, ReadBuf};

use super::{DriverWakeup, Lock, StreamError, StreamId};

use tokio_quiche::quic::QuicheConnection;

// "recvdrop" in ascii; if you see this then read everything or close(code)
// decimal: 7305813194079104880
const DROP_CODE: u64 = 0x6563766464726F70;

pub(crate) struct RecvState {
    id: StreamId,

    // Data that has been read and needs to be returned to the application.
    queued: VecDeque<Bytes>,

    // The amount of data that should be queued.
    max: usize,

    // The driver wakes up the application when data is available.
    blocked: Option<Waker>,

    // Set when STREAM_FIN
    fin: bool,

    // Set when RESET_STREAM is received
    reset: Option<u64>,

    // Set when STOP_SENDING is sent
    stop: Option<u64>,

    // Buffer for reading data.
    buf: BytesMut,

    // The size of the buffer doubles each time until it reaches the maximum size.
    buf_capacity: usize,
}

impl RecvState {
    pub fn new(id: StreamId) -> Self {
        Self {
            id,
            queued: Default::default(),
            max: 0,
            blocked: None,
            fin: false,
            reset: None,
            stop: None,
            buf: BytesMut::with_capacity(64),
            buf_capacity: 64,
        }
    }

    pub fn id(&self) -> StreamId {
        self.id
    }

    pub fn poll_read_chunk(
        &mut self,
        waker: &Waker,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, StreamError>> {
        println!("poll_read_chunk: {:?} {:?}", self.id, max);

        if let Some(reset) = self.reset {
            println!("returning reset: {:?} {:?}", self.id, reset);
            return Poll::Ready(Err(StreamError::Reset(reset)));
        }

        if let Some(stop) = self.stop {
            println!("returning stop: {:?} {:?}", self.id, stop);
            return Poll::Ready(Err(StreamError::Stop(stop)));
        }

        if let Some(mut chunk) = self.queued.pop_front() {
            if chunk.len() > max {
                let remain = chunk.split_off(max);
                self.queued.push_front(remain);
            }
            println!("returning chunk: {:?} {:?}", self.id, chunk.len());
            return Poll::Ready(Ok(Some(chunk)));
        }

        if self.fin {
            println!("returning fin: {:?}", self.id);
            return Poll::Ready(Ok(None));
        }

        // We'll return None if FIN, otherwise return an empty chunk.
        if max == 0 {
            return Poll::Ready(Ok(Some(Bytes::new())));
        }

        self.max = max;
        self.blocked = Some(waker.clone());
        println!("blocking for read: {:?}", self.id);

        Poll::Pending
    }

    pub fn poll_closed(&mut self, waker: &Waker) -> Poll<Result<(), StreamError>> {
        if self.fin && self.queued.is_empty() {
            Poll::Ready(Ok(()))
        } else if let Some(reset) = self.reset {
            Poll::Ready(Err(StreamError::Reset(reset)))
        } else if let Some(stop) = self.stop {
            Poll::Ready(Err(StreamError::Stop(stop)))
        } else {
            self.blocked = Some(waker.clone());
            Poll::Pending
        }
    }

    pub fn flush(&mut self, qconn: &mut QuicheConnection) -> quiche::Result<Option<Waker>> {
        if let Some(code) = self.reset {
            println!("already reset: {:?} {:?}", self.id, code);
            println!("TODO clean up");
            return Ok(self.blocked.take());
        }

        if let Some(stop) = self.stop {
            println!("shutting down recv: {:?} {:?}", self.id, stop);
            qconn.stream_shutdown(self.id.into(), quiche::Shutdown::Read, stop)?;
            return Ok(self.blocked.take());
        }

        let mut changed = false;

        while self.max > 0 {
            if self.buf.capacity() == 0 {
                // TODO get the readable size in Quiche so we can use that instead of guessing.
                self.buf_capacity = (self.buf_capacity * 2).min(32 * 1024);
                self.buf.reserve(self.buf_capacity);
            }

            // We don't actually use the buffer.len() because we immediately call split_to after reading.
            assert!(
                self.buf.is_empty(),
                "buffer should always be empty (but have capacity)"
            );

            // Do some unsafe to avoid zeroing the buffer.
            let buf: &mut [u8] = unsafe { std::mem::transmute(self.buf.spare_capacity_mut()) };
            let n = buf.len().min(self.max);

            match qconn.stream_recv(self.id.into(), &mut buf[..n]) {
                Ok((n, done)) => {
                    println!("received chunk: {:?} {:?} {:?}", self.id, n, done);
                    // Advance the buffer by the number of bytes read.
                    unsafe { self.buf.set_len(self.buf.len() + n) };

                    // Then split the buffer and push the front to the queue.
                    self.queued.push_back(self.buf.split_to(n).freeze());
                    self.max -= n;

                    changed = true;

                    println!("capacity after receiving: {:?} {:?}", self.id, self.max);

                    if done {
                        println!("setting fin: {:?}", self.id);
                        self.fin = true;
                        return Ok(self.blocked.take());
                    }
                }
                Err(quiche::Error::Done) => {
                    if qconn.stream_finished(self.id.into()) {
                        self.fin = true;
                        println!("waking blocked for FIN: {:?}", self.id);
                        return Ok(self.blocked.take());
                    }
                    break;
                }
                Err(quiche::Error::StreamReset(code)) => {
                    println!("stream reset: {:?} {:?}", self.id, code);
                    self.reset = Some(code);
                    println!("waking blocked for stream reset: {:?}", self.id);
                    return Ok(self.blocked.take());
                }
                Err(e) => return Err(e.into()),
            }
        }

        if changed {
            println!("waking blocked for received chunk: {:?}", self.id);
            Ok(self.blocked.take())
        } else {
            // Don't wake up the application if nothing was received.
            Ok(None)
        }
    }
}

pub struct RecvStream {
    id: StreamId,
    state: Lock<RecvState>,
    wakeup: Lock<DriverWakeup>,
}

impl RecvStream {
    pub(crate) fn new(id: StreamId, state: Lock<RecvState>, wakeup: Lock<DriverWakeup>) -> Self {
        Self { id, state, wakeup }
    }

    pub fn id(&self) -> StreamId {
        self.id
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, StreamError> {
        Ok(self.read_chunk(buf.len()).await?.map(|chunk| {
            buf[..chunk.len()].copy_from_slice(&chunk);
            chunk.len()
        }))
    }

    pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, StreamError> {
        poll_fn(|cx| self.poll_read_chunk(cx.waker(), max)).await
    }

    fn poll_read_chunk(
        &mut self,
        waker: &Waker,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, StreamError>> {
        if let Poll::Ready(res) = self.state.lock().poll_read_chunk(waker, max) {
            return Poll::Ready(res);
        }

        // If we're blocked, tell the driver we want more data.
        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }

        Poll::Pending
    }

    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<Option<usize>, StreamError> {
        match self
            .read(unsafe { std::mem::transmute(buf.chunk_mut()) })
            .await?
        {
            Some(n) => {
                unsafe { buf.advance_mut(n) };
                println!("!!! read buf: {:?} {:?} !!!", self.id, n);
                Ok(Some(n))
            }
            None => Ok(None),
        }
    }

    pub async fn read_all(&mut self) -> Result<Bytes, StreamError> {
        let mut buf = BytesMut::new();
        println!("!!! reading all: {:?} !!!", self.id);
        loop {
            match self.read_buf(&mut buf).await? {
                Some(_) => continue,
                None => break,
            }
        }

        println!("!!! read all: {:?} {:?} !!!", self.id, buf.len());

        Ok(buf.freeze())
    }

    // Reset the stream with the given error code.
    pub fn close(&mut self, code: u64) {
        self.state.lock().stop = Some(code);

        let waker = self.wakeup.lock().waker(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Returns true if the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a STOP_SENDING via [Self::close]
    /// - We received a RESET_STREAM via [RecvStream::close]
    /// - We received a FIN via [SendStream::finish]
    pub fn is_closed(&self) -> bool {
        let state = self.state.lock();
        (state.fin && state.queued.is_empty()) || state.reset.is_some() || state.stop.is_some()
    }

    /// Block until the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a RESET_STREAM via [Self::close]
    /// - We received a STOP_SENDING via [SendStream::close]
    /// - We received a FIN via [SendStream::finish]
    ///
    /// NOTE: This takes &mut to match Quinn and to simplify the implementation.
    pub async fn closed(&mut self) -> Result<(), StreamError> {
        poll_fn(|cx| self.state.lock().poll_closed(cx.waker())).await
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        let mut state = self.state.lock();

        if !state.fin && state.reset.is_none() && state.stop.is_none() {
            state.stop = Some(DROP_CODE);
            // Avoid two locks at once.
            drop(state);

            let waker = self.wakeup.lock().waker(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match ready!(self.poll_read_chunk(cx.waker(), buf.remaining())) {
            Ok(Some(chunk)) => buf.put_slice(&chunk),
            Ok(None) => {}
            Err(e) => return Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e.to_string()))),
        };
        Poll::Ready(Ok(()))
    }
}
