use std::sync::Arc;
use thiserror::Error;

/// An errors returned by [`Session`], split based on if they are underlying QUIC errors or WebTransport errors.
#[derive(Clone, Error, Debug)]
pub enum ConnectionError {
    #[error("quiche error: {0}")]
    Quiche(#[from] Arc<tokio_quiche::BoxError>),

    #[error("CONNECTION_CLOSE: code={0} reason={1}")]
    Closed(u64, String),
}

/// An error when writing to [`SendStream`].
#[derive(Clone, Error, Debug)]
pub enum SendError {
    #[error("connection error: {0}")]
    Connection(#[from] ConnectionError),

    #[error("STOP_SENDING: {0}")]
    Stop(u64),
}

/// An error when reading from [`RecvStream`].
#[derive(Clone, Error, Debug)]
pub enum RecvError {
    #[error("connection error: {0}")]
    Connection(#[from] ConnectionError),

    #[error("RESET_STREAM: {0}")]
    Reset(u64),

    #[error("stream closed")]
    Closed,
}

/// An error returned when receiving a new WebTransport session.
#[derive(Error, Debug, Clone)]
pub enum ServerError {
    #[error("quiche error: {0}")]
    Quiche(#[from] Arc<tokio_quiche::BoxError>),

    #[error("io error: {0}")]
    IoError(Arc<std::io::Error>),
}
