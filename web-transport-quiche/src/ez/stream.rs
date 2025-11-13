use std::sync::atomic::AtomicU64;
use thiserror::Error;

use super::ConnectionError;

/// An error when reading or writing to a stream.
#[derive(Clone, Error, Debug)]
pub enum StreamError {
    #[error("connection error: {0}")]
    Connection(#[from] ConnectionError),

    #[error("RESET_STREAM: {0}")]
    Reset(u64),

    #[error("STOP_SENDING: {0}")]
    Stop(u64),

    #[error("stream closed")]
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamId(u64);

impl StreamId {
    // The first stream IDs
    pub const CLIENT_BI: StreamId = StreamId(0);
    pub const SERVER_BI: StreamId = StreamId(1);
    pub const CLIENT_UNI: StreamId = StreamId(2);
    pub const SERVER_UNI: StreamId = StreamId(3);

    pub fn is_uni(&self) -> bool {
        // 2, 3, 6, 7, etc
        self.0 & 0b10 == 0b10
    }

    pub fn is_bi(&self) -> bool {
        !self.is_uni()
    }

    pub fn is_server(&self) -> bool {
        // 1, 3, 5, 7, etc
        self.0 & 0b01 == 0b01
    }

    pub fn is_client(&self) -> bool {
        !self.is_server()
    }

    pub fn increment(&mut self) -> StreamId {
        let id = *self;
        self.0 += 4;
        id
    }
}

impl From<StreamId> for AtomicU64 {
    fn from(id: StreamId) -> Self {
        AtomicU64::new(id.0)
    }
}

impl From<StreamId> for u64 {
    fn from(id: StreamId) -> Self {
        id.0
    }
}

impl From<u64> for StreamId {
    fn from(id: u64) -> Self {
        StreamId(id)
    }
}
