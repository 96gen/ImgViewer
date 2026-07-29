#![forbid(unsafe_code)]

use std::fmt;
use std::io::{self, Read, Write};

pub const PROTOCOL_MAGIC: [u8; 8] = *b"IMGVC001";
pub const PROTOCOL_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 16;
pub const MAX_CONTROL_PAYLOAD_BYTES: u32 = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum MessageKind {
    Hello = 1,
    Ready = 2,
    DecodeHeif = 3,
    NotImplemented = 4,
}

impl MessageKind {
    const fn code(self) -> u16 {
        self as u16
    }
}

impl TryFrom<u16> for MessageKind {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Ready),
            3 => Ok(Self::DecodeHeif),
            4 => Ok(Self::NotImplemented),
            _ => Err(ProtocolError::UnknownMessageKind(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    kind: MessageKind,
    payload_len: u32,
}

impl Header {
    pub fn new(kind: MessageKind, payload_len: u32) -> Result<Self, ProtocolError> {
        validate_payload_len(payload_len)?;
        Ok(Self { kind, payload_len })
    }

    pub const fn empty(kind: MessageKind) -> Self {
        Self {
            kind,
            payload_len: 0,
        }
    }

    pub const fn kind(self) -> MessageKind {
        self.kind
    }

    pub const fn payload_len(self) -> u32 {
        self.payload_len
    }

    pub fn encode(self) -> [u8; HEADER_LEN] {
        let mut bytes = [0_u8; HEADER_LEN];
        bytes[..8].copy_from_slice(&PROTOCOL_MAGIC);
        bytes[8..10].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&self.kind.code().to_le_bytes());
        bytes[12..16].copy_from_slice(&self.payload_len.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: [u8; HEADER_LEN]) -> Result<Self, ProtocolError> {
        if bytes[..8] != PROTOCOL_MAGIC {
            return Err(ProtocolError::InvalidMagic);
        }

        let version = u16::from_le_bytes([bytes[8], bytes[9]]);
        if version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }

        let kind = MessageKind::try_from(u16::from_le_bytes([bytes[10], bytes[11]]))?;
        let payload_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        Self::new(kind, payload_len)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    Io(io::ErrorKind),
    TruncatedHeader { bytes_read: usize },
    InvalidMagic,
    UnsupportedVersion(u16),
    UnknownMessageKind(u16),
    PayloadTooLarge { observed: u32, maximum: u32 },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(formatter, "codec protocol I/O error: {kind:?}"),
            Self::TruncatedHeader { bytes_read } => {
                write!(formatter, "truncated codec header after {bytes_read} bytes")
            }
            Self::InvalidMagic => formatter.write_str("invalid codec protocol magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported codec protocol version {version}")
            }
            Self::UnknownMessageKind(kind) => {
                write!(formatter, "unknown codec protocol message kind {kind}")
            }
            Self::PayloadTooLarge { observed, maximum } => write!(
                formatter,
                "codec control payload length {observed} exceeds {maximum}"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

pub fn read_header(reader: &mut (impl Read + ?Sized)) -> Result<Header, ProtocolError> {
    let mut bytes = [0_u8; HEADER_LEN];
    let mut bytes_read = 0;
    while bytes_read < HEADER_LEN {
        match reader.read(&mut bytes[bytes_read..]) {
            Ok(0) => return Err(ProtocolError::TruncatedHeader { bytes_read }),
            Ok(count) => bytes_read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(ProtocolError::Io(error.kind())),
        }
    }
    Header::decode(bytes)
}

pub fn write_header(
    writer: &mut (impl Write + ?Sized),
    header: Header,
) -> Result<(), ProtocolError> {
    writer
        .write_all(&header.encode())
        .map_err(|error| ProtocolError::Io(error.kind()))
}

fn validate_payload_len(payload_len: u32) -> Result<(), ProtocolError> {
    if payload_len > MAX_CONTROL_PAYLOAD_BYTES {
        return Err(ProtocolError::PayloadTooLarge {
            observed: payload_len,
            maximum: MAX_CONTROL_PAYLOAD_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn fixed_header_round_trips_without_allocation() {
        let header = Header::new(MessageKind::DecodeHeif, 32).unwrap();
        assert_eq!(Header::decode(header.encode()).unwrap(), header);
        assert_eq!(header.kind(), MessageKind::DecodeHeif);
        assert_eq!(header.payload_len(), 32);
    }

    #[test]
    fn invalid_magic_is_rejected() {
        let mut bytes = Header::empty(MessageKind::Hello).encode();
        bytes[0] ^= 0xff;
        assert_eq!(
            Header::decode(bytes).unwrap_err(),
            ProtocolError::InvalidMagic
        );
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let mut bytes = Header::empty(MessageKind::Hello).encode();
        bytes[8..10].copy_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes());
        assert_eq!(
            Header::decode(bytes).unwrap_err(),
            ProtocolError::UnsupportedVersion(PROTOCOL_VERSION + 1)
        );
    }

    #[test]
    fn unknown_message_kind_is_rejected() {
        let mut bytes = Header::empty(MessageKind::Hello).encode();
        bytes[10..12].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            Header::decode(bytes).unwrap_err(),
            ProtocolError::UnknownMessageKind(u16::MAX)
        );
    }

    #[test]
    fn oversized_control_payload_is_rejected_before_allocation() {
        let mut bytes = Header::empty(MessageKind::DecodeHeif).encode();
        let oversized = MAX_CONTROL_PAYLOAD_BYTES + 1;
        bytes[12..16].copy_from_slice(&oversized.to_le_bytes());
        assert_eq!(
            Header::decode(bytes).unwrap_err(),
            ProtocolError::PayloadTooLarge {
                observed: oversized,
                maximum: MAX_CONTROL_PAYLOAD_BYTES,
            }
        );
    }

    #[test]
    fn truncated_header_reports_observed_length() {
        let bytes = Header::empty(MessageKind::Hello).encode();
        let mut input = Cursor::new(&bytes[..HEADER_LEN - 1]);
        assert_eq!(
            read_header(&mut input).unwrap_err(),
            ProtocolError::TruncatedHeader {
                bytes_read: HEADER_LEN - 1,
            }
        );
    }

    #[test]
    fn header_io_round_trip_uses_exact_fixed_length() {
        let header = Header::empty(MessageKind::Ready);
        let mut bytes = Vec::new();
        write_header(&mut bytes, header).unwrap();
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(read_header(&mut Cursor::new(bytes)).unwrap(), header);
    }
}
