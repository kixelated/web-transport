use std::{
    collections::{HashMap, HashSet},
    future::{poll_fn, Future},
    task::{Poll, Waker},
};
use tokio_quiche::{
    buf_factory::{BufFactory, PooledBuf},
    quic::{HandshakeInfo, QuicheConnection},
    quiche,
};

use super::{
    ConnectionClosed, ConnectionError, Lock, Metrics, RecvState, RecvStream, SendState, SendStream,
    StreamId,
};

// Streams that need to be flushed to the quiche connection.
#[derive(Default)]
pub(crate) struct DriverWakeup {
    streams: HashSet<StreamId>,
    waker: Option<Waker>,
}

impl DriverWakeup {
    pub fn waker(&mut self, stream_id: StreamId) -> Option<Waker> {
        if !self.streams.insert(stream_id) {
            return None;
        }

        // You should call wake() without holding the lock.
        return self.waker.take();
    }
}

pub(crate) struct Driver {
    send: HashMap<StreamId, Lock<SendState>>,
    recv: HashMap<StreamId, Lock<RecvState>>,

    buf: PooledBuf,

    send_wakeup: Lock<DriverWakeup>,
    recv_wakeup: Lock<DriverWakeup>,

    accept_bi: flume::Sender<(SendStream, RecvStream)>,
    accept_uni: flume::Sender<RecvStream>,

    open_bi: flume::Receiver<(Lock<SendState>, Lock<RecvState>)>,
    open_uni: flume::Receiver<Lock<SendState>>,

    closed_local: ConnectionClosed,
    closed_remote: ConnectionClosed,
}

impl Driver {
    pub fn new(
        // Super gross, we should consolidate
        send_wakeup: Lock<DriverWakeup>,
        recv_wakeup: Lock<DriverWakeup>,
        accept_bi: flume::Sender<(SendStream, RecvStream)>,
        accept_uni: flume::Sender<RecvStream>,
        open_bi: flume::Receiver<(Lock<SendState>, Lock<RecvState>)>,
        open_uni: flume::Receiver<Lock<SendState>>,
        closed_local: ConnectionClosed,
        closed_remote: ConnectionClosed,
    ) -> Self {
        Self {
            send: HashMap::new(),
            recv: HashMap::new(),
            buf: BufFactory::get_max_buf(),
            send_wakeup,
            recv_wakeup,
            accept_bi,
            accept_uni,
            open_bi,
            open_uni,
            closed_local,
            closed_remote,
        }
    }

    fn connected(
        &mut self,
        qconn: &mut QuicheConnection,
        _handshake_info: &HandshakeInfo,
    ) -> Result<(), ConnectionError> {
        // Run poll once to advance any pending operations.
        match self.poll(Waker::noop(), qconn) {
            Poll::Ready(Err(e)) => Err(e),
            _ => Ok(()),
        }
    }

    fn read(&mut self, qconn: &mut QuicheConnection) -> Result<(), ConnectionError> {
        while let Some(stream_id) = qconn.stream_readable_next() {
            let stream_id = StreamId::from(stream_id);

            if let Some(entry) = self.recv.get_mut(&stream_id) {
                // Wake after dropping the lock to avoid deadlock
                let waker = entry.lock().flush(qconn)?;
                if let Some(waker) = waker {
                    waker.wake();
                }

                continue;
            }

            let mut state = RecvState::new(stream_id);
            state.flush(qconn)?; // no waker will be returned

            let state = Lock::new(state, "RecvState");
            self.recv.insert(stream_id, state.clone());
            let recv = RecvStream::new(stream_id, state.clone(), self.recv_wakeup.clone());

            if stream_id.is_bi() {
                let mut state = SendState::new(stream_id);
                state.flush(qconn)?; // no waker will be returned

                let state = Lock::new(state, "SendState");
                self.send.insert(stream_id, state.clone());

                let send = SendStream::new(stream_id, state.clone(), self.send_wakeup.clone());
                self.accept_bi
                    .send((send, recv))
                    .map_err(|_| ConnectionError::Dropped)?;
            } else {
                self.accept_uni
                    .send(recv)
                    .map_err(|_| ConnectionError::Dropped)?;
            }
        }

        Ok(())
    }

    fn write(&mut self, qconn: &mut QuicheConnection) -> Result<(), ConnectionError> {
        while let Some(stream_id) = qconn.stream_writable_next() {
            let stream_id = StreamId::from(stream_id);

            if let Some(state) = self.send.get_mut(&stream_id) {
                let waker = state.lock().flush(qconn)?;
                if let Some(waker) = waker {
                    waker.wake();
                }
            } else {
                return Err(quiche::Error::InvalidStreamState(stream_id.into()).into());
            }
        }

        Ok(())
    }

    async fn wait(&mut self, qconn: &mut QuicheConnection) -> Result<(), ConnectionError> {
        poll_fn(|cx| self.poll(cx.waker(), qconn)).await
    }

    fn poll(
        &mut self,
        waker: &Waker,
        qconn: &mut QuicheConnection,
    ) -> Poll<Result<(), ConnectionError>> {
        if !qconn.is_draining() {
            // Check if the application wants to close the connection.
            if let Poll::Ready(err) = self.closed_local.poll(waker) {
                // Close the connection and return the error.
                return Poll::Ready(
                    match err {
                        ConnectionError::Local(code, reason) => {
                            qconn.close(true, code, reason.as_bytes())
                        }
                        ConnectionError::Dropped => qconn.close(true, 0, b"dropped"),
                        ConnectionError::Remote(code, reason) => {
                            // This shouldn't happen, but just echo it back in case.
                            qconn.close(true, code, reason.as_bytes())
                        }
                        ConnectionError::Quiche(e) => {
                            qconn.close(true, 500, e.to_string().as_bytes())
                        }
                        ConnectionError::Unknown(reason) => {
                            qconn.close(true, 501, reason.as_bytes())
                        }
                    }
                    .map_err(ConnectionError::Quiche),
                );
            }
        }

        // Don't try to do anything during the handshake.
        if !qconn.is_established() {
            return Poll::Pending;
        }

        // Decide if we should poll or return to iterate the IO loop.
        let mut wait = true;

        // We're allowed to process recv messages when the connection is draining.
        {
            let mut recv = self.recv_wakeup.lock();

            // Register our waker for future wakeups.
            recv.waker = Some(waker.clone());

            // Make sure we drop the lock before processing.
            // Otherwise, we can cause a deadlock trying to access multiple locks at once.
            let streams = std::mem::take(&mut recv.streams);
            drop(recv);

            for stream_id in streams {
                if let Some(stream) = self.recv.get_mut(&stream_id) {
                    let waker = stream.lock().flush(qconn)?;
                    if let Some(waker) = waker {
                        waker.wake();
                    }

                    wait = false;
                }
            }
        }

        // Don't try to send/open during the draining or closed state.
        if qconn.is_draining() || qconn.is_closed() {
            if wait {
                return Poll::Pending;
            } else {
                return Poll::Ready(Ok(()));
            }
        }

        {
            let mut send = self.send_wakeup.lock();
            send.waker = Some(waker.clone());

            // Make sure we drop the lock before processing.
            // Otherwise, we can cause a deadlock trying to access multiple locks at once.
            let streams = std::mem::take(&mut send.streams);
            drop(send);

            for stream_id in streams {
                if let Some(stream) = self.send.get_mut(&stream_id) {
                    let waker = stream.lock().flush(qconn)?;
                    if let Some(waker) = waker {
                        waker.wake();
                    }

                    wait = false;
                }
            }
        }

        while qconn.peer_streams_left_bidi() > 0 {
            if let Ok((send, recv)) = self.open_bi.try_recv() {
                self.open_bi(qconn, send, recv)?;
                wait = false;
            } else {
                break;
            }
        }

        while qconn.peer_streams_left_uni() > 0 {
            if let Ok(recv) = self.open_uni.try_recv() {
                self.open_uni(qconn, recv)?;
                wait = false;
            } else {
                break;
            }
        }

        if wait {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn open_bi(
        &mut self,
        qconn: &mut QuicheConnection,
        send: Lock<SendState>,
        recv: Lock<RecvState>,
    ) -> Result<(), ConnectionError> {
        let id = {
            let mut state = send.lock();
            let id = state.id();
            qconn.stream_send(id.into(), &[], false)?;
            let waker = state.flush(qconn)?;
            drop(state);
            if let Some(waker) = waker {
                waker.wake();
            }
            id
        };
        self.send.insert(id, send);

        let id = {
            let mut state = recv.lock();
            let id = state.id();
            let waker = state.flush(qconn)?;
            drop(state);
            if let Some(waker) = waker {
                waker.wake();
            }
            id
        };
        self.recv.insert(id, recv);

        Ok(())
    }

    fn open_uni(
        &mut self,
        qconn: &mut QuicheConnection,
        send: Lock<SendState>,
    ) -> Result<(), ConnectionError> {
        let id = {
            let mut state = send.lock();
            let id = state.id();
            qconn.stream_send(id.into(), &[], false)?;
            let waker = state.flush(qconn)?;
            drop(state);
            if let Some(waker) = waker {
                waker.wake();
            }
            id
        };
        self.send.insert(id, send);

        Ok(())
    }

    fn abort(&mut self, err: ConnectionError) {
        let wakers = self.closed_local.abort(err);
        for waker in wakers {
            waker.wake();
        }
    }
}

impl tokio_quiche::ApplicationOverQuic for Driver {
    fn on_conn_established(
        &mut self,
        qconn: &mut QuicheConnection,
        handshake_info: &tokio_quiche::quic::HandshakeInfo,
    ) -> tokio_quiche::QuicResult<()> {
        if let Err(e) = self.connected(qconn, handshake_info) {
            self.abort(e);
        }

        Ok(())
    }

    fn should_act(&self) -> bool {
        // TODO
        true
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.buf
    }

    fn wait_for_data(
        &mut self,
        qconn: &mut QuicheConnection,
    ) -> impl Future<Output = Result<(), tokio_quiche::BoxError>> + Send {
        async {
            if let Err(e) = self.wait(qconn).await {
                self.abort(e.clone());
            }

            Ok(())
        }
    }

    fn process_reads(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        if let Err(e) = self.read(qconn) {
            self.abort(e);
        }

        Ok(())
    }

    fn process_writes(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        if let Err(e) = self.write(qconn) {
            self.abort(e);
        }

        Ok(())
    }

    fn on_conn_close<M: Metrics>(
        &mut self,
        qconn: &mut QuicheConnection,
        _metrics: &M,
        connection_result: &tokio_quiche::QuicResult<()>,
    ) {
        let err = if let Poll::Ready(err) = self.closed_local.poll(Waker::noop()) {
            err
        } else if let Some(local) = qconn.local_error() {
            let reason = String::from_utf8_lossy(&local.reason).to_string();
            ConnectionError::Local(local.error_code, reason)
        } else if let Some(peer) = qconn.peer_error() {
            let reason = String::from_utf8_lossy(&peer.reason).to_string();
            ConnectionError::Remote(peer.error_code, reason)
        } else if let Err(err) = connection_result {
            ConnectionError::Unknown(err.to_string())
        } else {
            ConnectionError::Unknown("no error message".to_string())
        };

        // Finally set the remote error once the connection is done.
        let wakers = self.closed_remote.abort(err);
        for waker in wakers {
            waker.wake();
        }
    }
}
