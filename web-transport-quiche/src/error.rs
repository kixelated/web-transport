use std::sync::Arc;
use thiserror::Error;
use tokio_quiche::quiche;
use web_transport_proto::{ConnectError, SettingsError};

/// An error returned when connecting to a WebTransport endpoint.
#[derive(Error, Debug, Clone)]
pub enum ClientError {
    #[error("unexpected end of stream")]
    UnexpectedEnd,

    #[error("quiche error: {0}")]
    Quiche(QuicheError),

    #[error("invalid DNS name: {0}")]
    InvalidDnsName(String),

    #[error("io error: {0}")]
    IoError(Arc<std::io::Error>),
}

/// An errors returned by [`Session`], split based on if they are underlying QUIC errors or WebTransport errors.
#[derive(Clone, Error, Debug)]
pub enum SessionError {
    #[error("quiche error: {0}")]
    Quiche(QuicheError),

    #[error("webtransport error: {0}")]
    WebTransport(#[from] WebTransportError),

    #[error("SETTINGS error: {0}")]
    Settings(#[from] SettingsError),

    #[error("CONNECT error: {0}")]
    Connect(#[from] ConnectError),

    #[error("closed")]
    Closed,

    #[error("pending")]
    Pending,

    #[error("unknown")]
    Unknown,
}

/// An error that can occur when reading/writing the WebTransport stream header.
#[derive(Clone, Error, Debug)]
pub enum WebTransportError {
    #[error("closed: code={0} reason={1}")]
    Closed(u32, String),

    #[error("unknown session")]
    UnknownSession,

    #[error("invalid stream header")]
    InvalidHeader,
}

/// An error when writing to [`SendStream`].
#[derive(Clone, Error, Debug)]
pub enum WriteError {
    #[error("STOP_SENDING: {0}")]
    Stopped(u32),

    #[error("invalid STOP_SENDING")]
    InvalidStopped,

    #[error("session error: {0}")]
    SessionError(#[from] SessionError),

    #[error("stream closed")]
    ClosedStream,

    #[error("would block")]
    WouldBlock,
}

/// An error when reading from [`RecvStream`].
#[derive(Clone, Error, Debug)]
pub enum ReadError {
    #[error("session error: {0}")]
    SessionError(#[from] SessionError),

    #[error("RESET_STREAM: {0}")]
    Reset(u32),

    #[error("invalid RESET_STREAM")]
    InvalidReset,

    #[error("stream already closed")]
    ClosedStream,

    #[error("would block")]
    WouldBlock,
}

/// An error returned by [`RecvStream::read_exact`].
#[derive(Clone, Error, Debug)]
pub enum ReadExactError {
    #[error("finished early")]
    FinishedEarly(usize),

    #[error("read error: {0}")]
    ReadError(#[from] ReadError),
}

/// An error returned by [`RecvStream::read_to_end`].
#[derive(Clone, Error, Debug)]
pub enum ReadToEndError {
    #[error("too long")]
    TooLong,

    #[error("read error: {0}")]
    ReadError(#[from] ReadError),
}

/// An error indicating the stream was already closed.
#[derive(Clone, Error, Debug)]
#[error("stream closed")]
pub struct ClosedStream;

/// An error returned when receiving a new WebTransport session.
#[derive(Error, Debug, Clone)]
pub enum ServerError {
    #[error("unexpected end of stream")]
    UnexpectedEnd,

    #[error("quiche error: {0}")]
    Quiche(QuicheError),

    #[error("io error: {0}")]
    IoError(Arc<std::io::Error>),
}

/// Wrapper around quiche::Error that implements Clone
#[derive(Error, Debug, Clone)]
#[error("{0:?}")]
pub struct QuicheError(pub Arc<quiche::Error>);

impl From<quiche::Error> for QuicheError {
    fn from(e: quiche::Error) -> Self {
        QuicheError(Arc::new(e))
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::IoError(Arc::new(e))
    }
}

impl From<std::io::Error> for ServerError {
    fn from(e: std::io::Error) -> Self {
        ServerError::IoError(Arc::new(e))
    }
}

impl From<quiche::Error> for ClientError {
    fn from(e: quiche::Error) -> Self {
        ClientError::Quiche(e.into())
    }
}

impl From<quiche::Error> for ServerError {
    fn from(e: quiche::Error) -> Self {
        ServerError::Quiche(e.into())
    }
}

impl From<quiche::Error> for SessionError {
    fn from(e: quiche::Error) -> Self {
        SessionError::Quiche(e.into())
    }
}

impl web_transport_trait::Error for SessionError {}
impl web_transport_trait::Error for WriteError {}
impl web_transport_trait::Error for ReadError {}
