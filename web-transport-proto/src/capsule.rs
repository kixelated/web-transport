use bytes::{Buf, BufMut, Bytes};

use crate::{VarInt, VarIntUnexpectedEnd};

// The spec (draft-ietf-webtrans-http3-06) says the type is 0x2843, which would
// varint-encode to 0x68 0x43. However, actual wire data shows 0x43 0x28 which
// decodes to 808. There may be a discrepancy in implementations or specs.
// Using 0x2843 as specified in the standard.
const CLOSE_WEBTRANSPORT_SESSION_TYPE: u64 = 0x2843;
const MAX_MESSAGE_SIZE: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capsule {
    CloseWebTransportSession { code: u32, reason: String },
    Unknown { typ: VarInt, payload: Bytes },
}

impl Capsule {
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self, CapsuleError> {
        loop {
            let typ = VarInt::decode(buf)?;
            let length = VarInt::decode(buf)?;

            let mut payload = buf.take(length.into_inner() as usize);
            if payload.remaining() > MAX_MESSAGE_SIZE {
                return Err(CapsuleError::MessageTooLong);
            }

            if payload.remaining() < payload.limit() {
                return Err(CapsuleError::UnexpectedEnd);
            }

            match typ.into_inner() {
                CLOSE_WEBTRANSPORT_SESSION_TYPE => {
                    if payload.remaining() < 4 {
                        return Err(CapsuleError::UnexpectedEnd);
                    }

                    let error_code = payload.get_u32();

                    let message_len = payload.remaining();
                    if message_len > MAX_MESSAGE_SIZE {
                        return Err(CapsuleError::MessageTooLong);
                    }

                    let mut message_bytes = vec![0u8; message_len];
                    payload.copy_to_slice(&mut message_bytes);

                    let error_message =
                        String::from_utf8(message_bytes).map_err(|_| CapsuleError::InvalidUtf8)?;

                    return Ok(Self::CloseWebTransportSession {
                        code: error_code,
                        reason: error_message,
                    });
                }
                t if is_grease(t) => continue,
                _ => {
                    // Unknown capsule type - store it
                    let mut payload_bytes = vec![0u8; payload.remaining()];
                    payload.copy_to_slice(&mut payload_bytes);
                    return Ok(Self::Unknown {
                        typ,
                        payload: Bytes::from(payload_bytes),
                    });
                }
            }
        }
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        match self {
            Self::CloseWebTransportSession {
                code: error_code,
                reason: error_message,
            } => {
                // Encode the capsule type
                VarInt::from_u64(CLOSE_WEBTRANSPORT_SESSION_TYPE)
                    .unwrap()
                    .encode(buf);

                // Calculate and encode the length
                let length = 4 + error_message.len();
                VarInt::from_u32(length as u32).encode(buf);

                // Encode the error code (32-bit)
                buf.put_u32(*error_code);

                // Encode the error message
                buf.put_slice(error_message.as_bytes());
            }
            Self::Unknown { typ, payload } => {
                // Encode the capsule type
                typ.encode(buf);

                // Encode the length
                VarInt::try_from(payload.len()).unwrap().encode(buf);

                // Encode the payload
                buf.put_slice(payload);
            }
        }
    }
}

fn is_grease(val: u64) -> bool {
    if val < 0x21 {
        return false;
    }
    #[allow(clippy::manual_is_multiple_of)]
    {
        (val - 0x21) % 0x1f == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapsuleError {
    #[error("unexpected end of buffer")]
    UnexpectedEnd,

    #[error("invalid UTF-8")]
    InvalidUtf8,

    #[error("message too long")]
    MessageTooLong,

    #[error("unknown capsule type: {0:?}")]
    UnknownType(VarInt),

    #[error("varint decode error: {0:?}")]
    VarInt(#[from] VarIntUnexpectedEnd),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn test_close_webtransport_session_decode() {
        // Test with spec-compliant type 0x2843 (encodes as 0x68 0x43)
        let mut data = Vec::new();
        VarInt::from_u64(0x2843).unwrap().encode(&mut data);
        VarInt::from_u32(8).encode(&mut data);
        data.extend_from_slice(b"\x00\x00\x01\xa4test");

        let mut buf = data.as_slice();
        let capsule = Capsule::decode(&mut buf).unwrap();

        match capsule {
            Capsule::CloseWebTransportSession {
                code: error_code,
                reason: error_message,
            } => {
                assert_eq!(error_code, 420);
                assert_eq!(error_message, "test");
            }
            _ => panic!("Expected CloseWebTransportSession"),
        }

        assert_eq!(buf.len(), 0); // All bytes consumed
    }

    #[test]
    fn test_close_webtransport_session_encode() {
        let capsule = Capsule::CloseWebTransportSession {
            code: 420,
            reason: "test".to_string(),
        };

        let mut buf = Vec::new();
        capsule.encode(&mut buf);

        // Expected format: type(0x2843 as varint = 0x68 0x43) + length(8 as varint) + error_code(420 as u32 BE) + "test"
        assert_eq!(buf, b"\x68\x43\x08\x00\x00\x01\xa4test");
    }

    #[test]
    fn test_close_webtransport_session_roundtrip() {
        let original = Capsule::CloseWebTransportSession {
            code: 12345,
            reason: "Connection closed by application".to_string(),
        };

        let mut buf = Vec::new();
        original.encode(&mut buf);

        let mut read_buf = buf.as_slice();
        let decoded = Capsule::decode(&mut read_buf).unwrap();

        assert_eq!(original, decoded);
        assert_eq!(read_buf.len(), 0); // All bytes consumed
    }

    #[test]
    fn test_empty_error_message() {
        let capsule = Capsule::CloseWebTransportSession {
            code: 0,
            reason: String::new(),
        };

        let mut buf = Vec::new();
        capsule.encode(&mut buf);

        // Type(0x2843 as varint = 0x68 0x43) + Length(4) + error_code(0)
        assert_eq!(buf, b"\x68\x43\x04\x00\x00\x00\x00");

        let mut read_buf = buf.as_slice();
        let decoded = Capsule::decode(&mut read_buf).unwrap();
        assert_eq!(capsule, decoded);
    }

    #[test]
    fn test_invalid_utf8() {
        // Create a capsule with invalid UTF-8 in the message
        let mut data = Vec::new();
        VarInt::from_u64(0x2843).unwrap().encode(&mut data); // type
        VarInt::from_u32(5).encode(&mut data); // length(5)
        data.extend_from_slice(b"\x00\x00\x00\x00"); // error_code(0)
        data.push(0xFF); // Invalid UTF-8 byte

        let mut buf = data.as_slice();
        let result = Capsule::decode(&mut buf);
        assert!(matches!(result, Err(CapsuleError::InvalidUtf8)));
    }

    #[test]
    fn test_truncated_error_code() {
        // Capsule with length indicating 3 bytes but error code needs 4
        let mut data = Vec::new();
        VarInt::from_u64(0x2843).unwrap().encode(&mut data); // type
        VarInt::from_u32(3).encode(&mut data); // length(3)
        data.extend_from_slice(b"\x00\x00\x00"); // incomplete error code

        let mut buf = data.as_slice();
        let result = Capsule::decode(&mut buf);
        assert!(matches!(result, Err(CapsuleError::UnexpectedEnd)));
    }

    #[test]
    fn test_unknown_capsule() {
        // Test handling of unknown capsule types
        let unknown_type = 0x1234u64;
        let payload_data = b"unknown payload";

        let mut data = Vec::new();
        VarInt::from_u64(unknown_type).unwrap().encode(&mut data);
        VarInt::from_u32(payload_data.len() as u32).encode(&mut data);
        data.extend_from_slice(payload_data);

        let mut buf = data.as_slice();
        let capsule = Capsule::decode(&mut buf).unwrap();

        match capsule {
            Capsule::Unknown { typ, payload } => {
                assert_eq!(typ.into_inner(), unknown_type);
                assert_eq!(payload.as_ref(), payload_data);
            }
            _ => panic!("Expected Unknown capsule"),
        }
    }

    #[test]
    fn test_unknown_capsule_roundtrip() {
        let capsule = Capsule::Unknown {
            typ: VarInt::from_u64(0x9999).unwrap(),
            payload: Bytes::from("test payload"),
        };

        let mut buf = Vec::new();
        capsule.encode(&mut buf);

        let mut read_buf = buf.as_slice();
        let decoded = Capsule::decode(&mut read_buf).unwrap();

        assert_eq!(capsule, decoded);
        assert_eq!(read_buf.len(), 0);
    }
}
