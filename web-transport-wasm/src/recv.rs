use std::cmp;

use bytes::{BufMut, Bytes, BytesMut};
use js_sys::Uint8Array;
use web_sys::WebTransportReceiveStream;

use crate::{ReadError, Reader};

pub struct RecvStream {
    reader: Reader,
    buffer: BytesMut,
}

impl RecvStream {
    pub(super) fn new(stream: WebTransportReceiveStream) -> Result<Self, ReadError> {
        let reader = Reader::new(&stream)?;

        Ok(Self {
            reader,
            buffer: BytesMut::new(),
        })
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ReadError> {
        let chunk = match self.read_chunk(buf.len()).await? {
            Some(chunk) => chunk,
            None => return Ok(None),
        };

        let size = chunk.len();
        buf[..size].copy_from_slice(&chunk);
        Ok(Some(size))
    }

    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<Option<usize>, ReadError> {
        let chunk = match self.read_chunk(buf.remaining_mut()).await? {
            Some(chunk) => chunk,
            None => return Ok(None),
        };

        let size = chunk.len();
        buf.put(chunk);

        Ok(Some(size))
    }

    pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, ReadError> {
        if !self.buffer.is_empty() {
            let size = cmp::min(max, self.buffer.len());
            let data = self.buffer.split_to(size).freeze();
            return Ok(Some(data));
        }

        let mut data: Bytes = match self.reader.read::<Uint8Array>().await? {
            // TODO can we avoid making a copy here?
            Some(data) => data.to_vec().into(),
            None => return Ok(None),
        };

        if data.len() > max {
            // The chunk is too big; add the tail to the buffer for next read.
            self.buffer.extend_from_slice(&data.split_off(max));
        }

        Ok(Some(data))
    }

    pub fn stop(&mut self, reason: &str) {
        self.reader.close(reason);
    }
}
