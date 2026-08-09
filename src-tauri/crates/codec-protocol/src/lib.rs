#![forbid(unsafe_code)]

use std::fmt;
use std::io::{self, Read, Write};

pub const PROTOCOL_MAGIC: [u8; 8] = *b"IMGVC001";
pub const PROTOCOL_VERSION: u16 = 3;
pub const CODEC_HELPER_MEMORY_LIMIT_BYTES: usize = 805_306_368;
pub const CODEC_HELPER_DECODE_DEADLINE_MS: u64 = 30_000;
pub const HEADER_LEN: usize = 16;
pub const MAX_CONTROL_PAYLOAD_BYTES: u32 = 64 * 1024;
pub const MAX_RENDER_BYTES: u32 = 512 * 1024 * 1024;
pub const MAX_RENDER_SIDE: u32 = 32_768;
pub const MAX_RENDER_PIXELS: u64 = 100_000_000;
pub const DECODE_REQUEST_LEN: u32 = 32;
pub const DECODE_SUCCESS_PREFIX_LEN: u32 = 24;
pub const DECODE_ERROR_LEN: u32 = 32;
pub const MAX_RENDER_PAYLOAD_BYTES: u32 = MAX_RENDER_BYTES + DECODE_SUCCESS_PREFIX_LEN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum MessageKind {
    Hello = 1,
    Ready = 2,
    DecodeImage = 3,
    DecodeSuccess = 4,
    DecodeError = 5,
    Shutdown = 6,
}

impl MessageKind {
    pub const fn code(self) -> u16 {
        self as u16
    }
}

impl TryFrom<u16> for MessageKind {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Ready),
            3 => Ok(Self::DecodeImage),
            4 => Ok(Self::DecodeSuccess),
            5 => Ok(Self::DecodeError),
            6 => Ok(Self::Shutdown),
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
        validate_header_payload(kind, payload_len)?;
        Ok(Self { kind, payload_len })
    }

    pub const fn hello() -> Self {
        Self {
            kind: MessageKind::Hello,
            payload_len: 0,
        }
    }

    pub const fn ready() -> Self {
        Self {
            kind: MessageKind::Ready,
            payload_len: 0,
        }
    }

    pub const fn shutdown() -> Self {
        Self {
            kind: MessageKind::Shutdown,
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
#[repr(u16)]
pub enum CodecFormat {
    Heif = 1,
    Tiff = 2,
}

impl CodecFormat {
    pub const fn code(self) -> u16 {
        self as u16
    }
}

impl TryFrom<u16> for CodecFormat {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Heif),
            2 => Ok(Self::Tiff),
            _ => Err(ProtocolError::UnknownCodecFormat(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeRequest {
    pub request_id: u64,
    pub duplicated_handle: u64,
    pub expected_length: u64,
    pub format: CodecFormat,
}

impl DecodeRequest {
    pub fn encode(self) -> [u8; DECODE_REQUEST_LEN as usize] {
        let mut bytes = [0_u8; DECODE_REQUEST_LEN as usize];
        bytes[..8].copy_from_slice(&self.request_id.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.duplicated_handle.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.expected_length.to_le_bytes());
        bytes[24..26].copy_from_slice(&self.format.code().to_le_bytes());
        bytes
    }

    pub fn decode(bytes: [u8; DECODE_REQUEST_LEN as usize]) -> Result<Self, ProtocolError> {
        if bytes[26..32] != [0_u8; 6] {
            return Err(ProtocolError::NonCanonicalReservedBytes);
        }
        Ok(Self {
            request_id: u64::from_le_bytes(bytes[..8].try_into().expect("fixed request id")),
            duplicated_handle: u64::from_le_bytes(
                bytes[8..16].try_into().expect("fixed duplicated handle"),
            ),
            expected_length: u64::from_le_bytes(
                bytes[16..24].try_into().expect("fixed expected length"),
            ),
            format: CodecFormat::try_from(u16::from_le_bytes([bytes[24], bytes[25]]))?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum WireErrorCode {
    CorruptImage = 1,
    FormatMismatch = 2,
    FileTooLarge = 3,
    DimensionsExceeded = 4,
    DecodeLimitExceeded = 5,
    UnsupportedBitDepth = 6,
    UnsupportedColorProfile = 7,
    IoError = 8,
    InternalDecoderError = 9,
    NotImplemented = 10,
    InvalidHandle = 11,
}

impl WireErrorCode {
    pub const fn code(self) -> u16 {
        self as u16
    }
}

impl TryFrom<u16> for WireErrorCode {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::CorruptImage),
            2 => Ok(Self::FormatMismatch),
            3 => Ok(Self::FileTooLarge),
            4 => Ok(Self::DimensionsExceeded),
            5 => Ok(Self::DecodeLimitExceeded),
            6 => Ok(Self::UnsupportedBitDepth),
            7 => Ok(Self::UnsupportedColorProfile),
            8 => Ok(Self::IoError),
            9 => Ok(Self::InternalDecoderError),
            10 => Ok(Self::NotImplemented),
            11 => Ok(Self::InvalidHandle),
            _ => Err(ProtocolError::UnknownWireErrorCode(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeError {
    pub request_id: u64,
    pub code: WireErrorCode,
    pub arg0: u64,
    pub arg1: u64,
}

impl DecodeError {
    pub fn encode(self) -> [u8; DECODE_ERROR_LEN as usize] {
        let mut bytes = [0_u8; DECODE_ERROR_LEN as usize];
        bytes[..8].copy_from_slice(&self.request_id.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.code.code().to_le_bytes());
        bytes[16..24].copy_from_slice(&self.arg0.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.arg1.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: [u8; DECODE_ERROR_LEN as usize]) -> Result<Self, ProtocolError> {
        if bytes[10..16] != [0_u8; 6] {
            return Err(ProtocolError::NonCanonicalReservedBytes);
        }
        Ok(Self {
            request_id: u64::from_le_bytes(bytes[..8].try_into().expect("fixed request id")),
            code: WireErrorCode::try_from(u16::from_le_bytes([bytes[8], bytes[9]]))?,
            arg0: u64::from_le_bytes(bytes[16..24].try_into().expect("fixed error arg0")),
            arg1: u64::from_le_bytes(bytes[24..32].try_into().expect("fixed error arg1")),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeSuccess {
    pub request_id: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeResponse {
    Success(DecodeSuccess),
    Error(DecodeError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperCommand {
    DecodeImage(DecodeRequest),
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    Io(io::ErrorKind),
    TruncatedHeader {
        bytes_read: usize,
    },
    TruncatedPayload {
        expected: usize,
        bytes_read: usize,
    },
    InvalidMagic,
    UnsupportedVersion(u16),
    UnknownMessageKind(u16),
    UnknownCodecFormat(u16),
    UnknownWireErrorCode(u16),
    UnexpectedMessage {
        observed: MessageKind,
    },
    PayloadTooLarge {
        kind: MessageKind,
        observed: u32,
        maximum: u32,
    },
    InvalidPayloadLength {
        kind: MessageKind,
        observed: u32,
        minimum: u32,
        maximum: u32,
    },
    RequestIdMismatch {
        expected: u64,
        observed: u64,
    },
    AllocationFailed {
        requested: u32,
    },
    InvalidRenderDimensions {
        width: u32,
        height: u32,
    },
    InvalidRgbaLength {
        expected: u32,
        observed: u32,
    },
    NonCanonicalReservedBytes,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(formatter, "codec protocol I/O error: {kind:?}"),
            Self::TruncatedHeader { bytes_read } => {
                write!(formatter, "truncated codec header after {bytes_read} bytes")
            }
            Self::TruncatedPayload {
                expected,
                bytes_read,
            } => write!(
                formatter,
                "truncated codec payload after {bytes_read} of {expected} bytes"
            ),
            Self::InvalidMagic => formatter.write_str("invalid codec protocol magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported codec protocol version {version}")
            }
            Self::UnknownMessageKind(kind) => {
                write!(formatter, "unknown codec protocol message kind {kind}")
            }
            Self::UnknownCodecFormat(format) => {
                write!(formatter, "unknown codec format {format}")
            }
            Self::UnknownWireErrorCode(code) => {
                write!(formatter, "unknown codec wire error code {code}")
            }
            Self::UnexpectedMessage { observed } => {
                write!(formatter, "unexpected codec message {observed:?}")
            }
            Self::PayloadTooLarge {
                kind,
                observed,
                maximum,
            } => write!(
                formatter,
                "{kind:?} payload length {observed} exceeds {maximum}"
            ),
            Self::InvalidPayloadLength {
                kind,
                observed,
                minimum,
                maximum,
            } => write!(
                formatter,
                "{kind:?} payload length {observed} is outside {minimum}..={maximum}"
            ),
            Self::RequestIdMismatch { expected, observed } => write!(
                formatter,
                "codec response request id {observed} does not match {expected}"
            ),
            Self::AllocationFailed { requested } => {
                write!(
                    formatter,
                    "unable to allocate bounded {requested}-byte payload"
                )
            }
            Self::InvalidRenderDimensions { width, height } => {
                write!(formatter, "invalid render dimensions {width}x{height}")
            }
            Self::InvalidRgbaLength { expected, observed } => write!(
                formatter,
                "RGBA8 payload length {observed} does not match expected {expected}"
            ),
            Self::NonCanonicalReservedBytes => {
                formatter.write_str("non-zero reserved protocol bytes")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

pub fn read_header(reader: &mut (impl Read + ?Sized)) -> Result<Header, ProtocolError> {
    let mut bytes = [0_u8; HEADER_LEN];
    read_exact_counted(reader, &mut bytes, true)?;
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

pub fn write_hello(writer: &mut (impl Write + ?Sized)) -> Result<(), ProtocolError> {
    write_header(writer, Header::hello())
}

pub fn read_hello(reader: &mut (impl Read + ?Sized)) -> Result<(), ProtocolError> {
    require_message(read_header(reader)?, MessageKind::Hello)
}

pub fn write_ready(writer: &mut (impl Write + ?Sized)) -> Result<(), ProtocolError> {
    write_header(writer, Header::ready())
}

pub fn read_ready(reader: &mut (impl Read + ?Sized)) -> Result<(), ProtocolError> {
    require_message(read_header(reader)?, MessageKind::Ready)
}

pub fn write_shutdown(writer: &mut (impl Write + ?Sized)) -> Result<(), ProtocolError> {
    write_header(writer, Header::shutdown())
}

pub fn write_decode_request(
    writer: &mut (impl Write + ?Sized),
    request: DecodeRequest,
) -> Result<(), ProtocolError> {
    write_header(
        writer,
        Header::new(MessageKind::DecodeImage, DECODE_REQUEST_LEN)?,
    )?;
    writer
        .write_all(&request.encode())
        .map_err(|error| ProtocolError::Io(error.kind()))
}

pub fn read_helper_command(
    reader: &mut (impl Read + ?Sized),
) -> Result<HelperCommand, ProtocolError> {
    let header = read_header(reader)?;
    match header.kind() {
        MessageKind::DecodeImage => {
            let mut bytes = [0_u8; DECODE_REQUEST_LEN as usize];
            read_exact_counted(reader, &mut bytes, false)?;
            Ok(HelperCommand::DecodeImage(DecodeRequest::decode(bytes)?))
        }
        MessageKind::Shutdown => Ok(HelperCommand::Shutdown),
        observed => Err(ProtocolError::UnexpectedMessage { observed }),
    }
}

pub fn write_decode_error(
    writer: &mut (impl Write + ?Sized),
    error: DecodeError,
) -> Result<(), ProtocolError> {
    write_header(
        writer,
        Header::new(MessageKind::DecodeError, DECODE_ERROR_LEN)?,
    )?;
    writer
        .write_all(&error.encode())
        .map_err(|io_error| ProtocolError::Io(io_error.kind()))
}

pub fn write_decode_success(
    writer: &mut (impl Write + ?Sized),
    request_id: u64,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), ProtocolError> {
    let expected_rgba_len = validate_render_dimensions(width, height)?;
    let rgba_len = u32::try_from(rgba.len()).map_err(|_| ProtocolError::PayloadTooLarge {
        kind: MessageKind::DecodeSuccess,
        observed: u32::MAX,
        maximum: MAX_RENDER_BYTES,
    })?;
    if rgba_len > MAX_RENDER_BYTES {
        return Err(ProtocolError::PayloadTooLarge {
            kind: MessageKind::DecodeSuccess,
            observed: rgba_len,
            maximum: MAX_RENDER_BYTES,
        });
    }
    if rgba_len != expected_rgba_len {
        return Err(ProtocolError::InvalidRgbaLength {
            expected: expected_rgba_len,
            observed: rgba_len,
        });
    }
    let payload_len = DECODE_SUCCESS_PREFIX_LEN
        .checked_add(rgba_len)
        .expect("bounded render payload fits u32");
    write_header(
        writer,
        Header::new(MessageKind::DecodeSuccess, payload_len)?,
    )?;

    let mut prefix = [0_u8; DECODE_SUCCESS_PREFIX_LEN as usize];
    prefix[..8].copy_from_slice(&request_id.to_le_bytes());
    prefix[8..12].copy_from_slice(&width.to_le_bytes());
    prefix[12..16].copy_from_slice(&height.to_le_bytes());
    prefix[16..20].copy_from_slice(&rgba_len.to_le_bytes());
    writer
        .write_all(&prefix)
        .and_then(|_| writer.write_all(rgba))
        .map_err(|error| ProtocolError::Io(error.kind()))
}

pub fn read_decode_response(
    reader: &mut (impl Read + ?Sized),
    expected_request_id: u64,
) -> Result<DecodeResponse, ProtocolError> {
    let header = read_header(reader)?;
    match header.kind() {
        MessageKind::DecodeError => {
            let mut bytes = [0_u8; DECODE_ERROR_LEN as usize];
            read_exact_counted(reader, &mut bytes, false)?;
            let error = DecodeError::decode(bytes)?;
            validate_request_id(expected_request_id, error.request_id)?;
            Ok(DecodeResponse::Error(error))
        }
        MessageKind::DecodeSuccess => {
            let mut prefix = [0_u8; DECODE_SUCCESS_PREFIX_LEN as usize];
            read_exact_counted(reader, &mut prefix, false)?;
            if prefix[20..24] != [0_u8; 4] {
                return Err(ProtocolError::NonCanonicalReservedBytes);
            }

            let request_id = u64::from_le_bytes(prefix[..8].try_into().expect("fixed request id"));
            validate_request_id(expected_request_id, request_id)?;
            let width = u32::from_le_bytes(prefix[8..12].try_into().expect("fixed width"));
            let height = u32::from_le_bytes(prefix[12..16].try_into().expect("fixed height"));
            let expected_rgba_len = validate_render_dimensions(width, height)?;
            let rgba_len =
                u32::from_le_bytes(prefix[16..20].try_into().expect("fixed RGBA length"));
            if rgba_len > MAX_RENDER_BYTES {
                return Err(ProtocolError::PayloadTooLarge {
                    kind: MessageKind::DecodeSuccess,
                    observed: rgba_len,
                    maximum: MAX_RENDER_BYTES,
                });
            }
            if rgba_len != expected_rgba_len {
                return Err(ProtocolError::InvalidRgbaLength {
                    expected: expected_rgba_len,
                    observed: rgba_len,
                });
            }
            let expected_payload_len = DECODE_SUCCESS_PREFIX_LEN
                .checked_add(rgba_len)
                .expect("bounded render payload fits u32");
            if header.payload_len() != expected_payload_len {
                return Err(ProtocolError::InvalidPayloadLength {
                    kind: MessageKind::DecodeSuccess,
                    observed: header.payload_len(),
                    minimum: expected_payload_len,
                    maximum: expected_payload_len,
                });
            }

            let rgba_capacity =
                usize::try_from(rgba_len).expect("u32 render length fits supported hosts");
            let mut rgba = Vec::new();
            rgba.try_reserve_exact(rgba_capacity)
                .map_err(|_| ProtocolError::AllocationFailed {
                    requested: rgba_len,
                })?;
            rgba.resize(rgba_capacity, 0);
            read_exact_counted(reader, &mut rgba, false)?;
            Ok(DecodeResponse::Success(DecodeSuccess {
                request_id,
                width,
                height,
                rgba,
            }))
        }
        observed => Err(ProtocolError::UnexpectedMessage { observed }),
    }
}

fn validate_header_payload(kind: MessageKind, payload_len: u32) -> Result<(), ProtocolError> {
    if kind != MessageKind::DecodeSuccess && payload_len > MAX_CONTROL_PAYLOAD_BYTES {
        return Err(ProtocolError::PayloadTooLarge {
            kind,
            observed: payload_len,
            maximum: MAX_CONTROL_PAYLOAD_BYTES,
        });
    }
    if kind == MessageKind::DecodeSuccess && payload_len > MAX_RENDER_PAYLOAD_BYTES {
        return Err(ProtocolError::PayloadTooLarge {
            kind,
            observed: payload_len,
            maximum: MAX_RENDER_PAYLOAD_BYTES,
        });
    }

    let (minimum, maximum) = match kind {
        MessageKind::Hello | MessageKind::Ready | MessageKind::Shutdown => (0, 0),
        MessageKind::DecodeImage => (DECODE_REQUEST_LEN, DECODE_REQUEST_LEN),
        MessageKind::DecodeError => (DECODE_ERROR_LEN, DECODE_ERROR_LEN),
        MessageKind::DecodeSuccess => (DECODE_SUCCESS_PREFIX_LEN, MAX_RENDER_PAYLOAD_BYTES),
    };
    if payload_len < minimum || payload_len > maximum {
        return Err(ProtocolError::InvalidPayloadLength {
            kind,
            observed: payload_len,
            minimum,
            maximum,
        });
    }
    Ok(())
}

fn require_message(header: Header, expected: MessageKind) -> Result<(), ProtocolError> {
    if header.kind() != expected {
        return Err(ProtocolError::UnexpectedMessage {
            observed: header.kind(),
        });
    }
    Ok(())
}

fn validate_request_id(expected: u64, observed: u64) -> Result<(), ProtocolError> {
    if observed != expected {
        return Err(ProtocolError::RequestIdMismatch { expected, observed });
    }
    Ok(())
}

fn validate_render_dimensions(width: u32, height: u32) -> Result<u32, ProtocolError> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(ProtocolError::InvalidRenderDimensions { width, height })?;
    if width == 0
        || height == 0
        || width > MAX_RENDER_SIDE
        || height > MAX_RENDER_SIDE
        || pixels > MAX_RENDER_PIXELS
    {
        return Err(ProtocolError::InvalidRenderDimensions { width, height });
    }
    let rgba_len = pixels
        .checked_mul(4)
        .filter(|length| *length <= u64::from(MAX_RENDER_BYTES))
        .ok_or(ProtocolError::InvalidRenderDimensions { width, height })?;
    u32::try_from(rgba_len).map_err(|_| ProtocolError::InvalidRenderDimensions { width, height })
}

fn read_exact_counted(
    reader: &mut (impl Read + ?Sized),
    bytes: &mut [u8],
    header: bool,
) -> Result<(), ProtocolError> {
    let mut bytes_read = 0;
    while bytes_read < bytes.len() {
        match reader.read(&mut bytes[bytes_read..]) {
            Ok(0) if header => return Err(ProtocolError::TruncatedHeader { bytes_read }),
            Ok(0) => {
                return Err(ProtocolError::TruncatedPayload {
                    expected: bytes.len(),
                    bytes_read,
                });
            }
            Ok(count) => bytes_read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(ProtocolError::Io(error.kind())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn rgba(width: u32, height: u32) -> Vec<u8> {
        vec![0x5a; (width * height * 4) as usize]
    }

    #[test]
    fn fixed_header_and_decode_request_round_trip_uses_explicit_fields() {
        let request = DecodeRequest {
            request_id: 42,
            duplicated_handle: 0x1234,
            expected_length: 9_876,
            format: CodecFormat::Tiff,
        };
        let mut bytes = Vec::new();
        write_decode_request(&mut bytes, request).unwrap();
        assert_eq!(bytes.len(), HEADER_LEN + DECODE_REQUEST_LEN as usize);
        assert_eq!(&bytes[8..10], &PROTOCOL_VERSION.to_le_bytes());
        assert_eq!(
            &bytes[10..12],
            &MessageKind::DecodeImage.code().to_le_bytes()
        );
        assert_eq!(
            &bytes[HEADER_LEN + 24..HEADER_LEN + 26],
            &2_u16.to_le_bytes()
        );
        assert_eq!(&bytes[HEADER_LEN + 26..HEADER_LEN + 32], &[0_u8; 6]);
        assert_eq!(
            read_helper_command(&mut Cursor::new(bytes)).unwrap(),
            HelperCommand::DecodeImage(request)
        );
    }

    #[test]
    fn codec_format_codes_are_stable_and_unknown_values_fail_closed() {
        assert_eq!(CODEC_HELPER_MEMORY_LIMIT_BYTES, 805_306_368);
        assert_eq!(CODEC_HELPER_DECODE_DEADLINE_MS, 30_000);
        assert_eq!(CodecFormat::Heif.code(), 1);
        assert_eq!(CodecFormat::Tiff.code(), 2);
        assert_eq!(CodecFormat::try_from(1), Ok(CodecFormat::Heif));
        assert_eq!(CodecFormat::try_from(2), Ok(CodecFormat::Tiff));
        assert_eq!(
            CodecFormat::try_from(0).unwrap_err(),
            ProtocolError::UnknownCodecFormat(0)
        );

        let request = DecodeRequest {
            request_id: 7,
            duplicated_handle: 8,
            expected_length: 9,
            format: CodecFormat::Heif,
        };
        let mut bytes = Vec::new();
        write_decode_request(&mut bytes, request).unwrap();
        bytes[HEADER_LEN + 24..HEADER_LEN + 26].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            read_helper_command(&mut Cursor::new(bytes)).unwrap_err(),
            ProtocolError::UnknownCodecFormat(u16::MAX)
        );
    }

    #[test]
    fn decode_request_reserved_bytes_and_non_v3_length_fail_closed() {
        let request = DecodeRequest {
            request_id: 7,
            duplicated_handle: 8,
            expected_length: 9,
            format: CodecFormat::Tiff,
        };
        let mut noncanonical = Vec::new();
        write_decode_request(&mut noncanonical, request).unwrap();
        noncanonical[HEADER_LEN + 31] = 1;
        assert_eq!(
            read_helper_command(&mut Cursor::new(noncanonical)).unwrap_err(),
            ProtocolError::NonCanonicalReservedBytes
        );

        let mut v2_length = Header::new(MessageKind::DecodeImage, DECODE_REQUEST_LEN)
            .unwrap()
            .encode();
        v2_length[12..16].copy_from_slice(&24_u32.to_le_bytes());
        assert_eq!(
            read_helper_command(&mut Cursor::new(v2_length)).unwrap_err(),
            ProtocolError::InvalidPayloadLength {
                kind: MessageKind::DecodeImage,
                observed: 24,
                minimum: DECODE_REQUEST_LEN,
                maximum: DECODE_REQUEST_LEN,
            }
        );
    }

    #[test]
    fn invalid_magic_version_and_kind_are_rejected() {
        let mut invalid_magic = Header::hello().encode();
        invalid_magic[0] ^= 0xff;
        assert_eq!(
            Header::decode(invalid_magic).unwrap_err(),
            ProtocolError::InvalidMagic
        );

        let mut invalid_version = Header::hello().encode();
        invalid_version[8..10].copy_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes());
        assert_eq!(
            Header::decode(invalid_version).unwrap_err(),
            ProtocolError::UnsupportedVersion(PROTOCOL_VERSION + 1)
        );

        let mut invalid_kind = Header::hello().encode();
        invalid_kind[10..12].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            Header::decode(invalid_kind).unwrap_err(),
            ProtocolError::UnknownMessageKind(u16::MAX)
        );
    }

    #[test]
    fn control_and_render_oversize_are_rejected_from_headers_before_allocation() {
        let mut control = Header::hello().encode();
        control[12..16].copy_from_slice(&(MAX_CONTROL_PAYLOAD_BYTES + 1).to_le_bytes());
        assert_eq!(
            Header::decode(control).unwrap_err(),
            ProtocolError::PayloadTooLarge {
                kind: MessageKind::Hello,
                observed: MAX_CONTROL_PAYLOAD_BYTES + 1,
                maximum: MAX_CONTROL_PAYLOAD_BYTES,
            }
        );

        let mut render = Header::new(MessageKind::DecodeSuccess, DECODE_SUCCESS_PREFIX_LEN)
            .unwrap()
            .encode();
        render[12..16].copy_from_slice(&(MAX_RENDER_PAYLOAD_BYTES + 1).to_le_bytes());
        assert_eq!(
            Header::decode(render).unwrap_err(),
            ProtocolError::PayloadTooLarge {
                kind: MessageKind::DecodeSuccess,
                observed: MAX_RENDER_PAYLOAD_BYTES + 1,
                maximum: MAX_RENDER_PAYLOAD_BYTES,
            }
        );
    }

    #[test]
    fn truncated_header_and_payload_report_observed_lengths() {
        let header = Header::hello().encode();
        assert_eq!(
            read_header(&mut Cursor::new(&header[..HEADER_LEN - 1])).unwrap_err(),
            ProtocolError::TruncatedHeader {
                bytes_read: HEADER_LEN - 1
            }
        );

        let mut request = Vec::new();
        write_header(
            &mut request,
            Header::new(MessageKind::DecodeImage, DECODE_REQUEST_LEN).unwrap(),
        )
        .unwrap();
        request.extend_from_slice(&[0_u8; 5]);
        assert_eq!(
            read_helper_command(&mut Cursor::new(request)).unwrap_err(),
            ProtocolError::TruncatedPayload {
                expected: DECODE_REQUEST_LEN as usize,
                bytes_read: 5,
            }
        );
    }

    #[test]
    fn success_round_trip_validates_rgba_dimensions_length_and_request_id() {
        let rgba = rgba(3, 2);
        let mut response = Vec::new();
        write_decode_success(&mut response, 77, 3, 2, &rgba).unwrap();
        assert_eq!(
            read_decode_response(&mut Cursor::new(response.clone()), 77).unwrap(),
            DecodeResponse::Success(DecodeSuccess {
                request_id: 77,
                width: 3,
                height: 2,
                rgba,
            })
        );
        assert_eq!(
            read_decode_response(&mut Cursor::new(response), 78).unwrap_err(),
            ProtocolError::RequestIdMismatch {
                expected: 78,
                observed: 77,
            }
        );
    }

    #[test]
    fn mismatched_rgba_length_is_rejected_before_body_read_or_allocation() {
        assert_eq!(
            write_decode_success(&mut Vec::new(), 1, 1, 1, &[0_u8; 8]).unwrap_err(),
            ProtocolError::InvalidRgbaLength {
                expected: 4,
                observed: 8,
            }
        );

        let mut malicious_response = Vec::new();
        write_header(
            &mut malicious_response,
            Header::new(MessageKind::DecodeSuccess, DECODE_SUCCESS_PREFIX_LEN + 8).unwrap(),
        )
        .unwrap();
        let mut prefix = [0_u8; DECODE_SUCCESS_PREFIX_LEN as usize];
        prefix[..8].copy_from_slice(&1_u64.to_le_bytes());
        prefix[8..12].copy_from_slice(&1_u32.to_le_bytes());
        prefix[12..16].copy_from_slice(&1_u32.to_le_bytes());
        prefix[16..20].copy_from_slice(&8_u32.to_le_bytes());
        malicious_response.extend_from_slice(&prefix);
        assert_eq!(
            read_decode_response(&mut Cursor::new(malicious_response), 1).unwrap_err(),
            ProtocolError::InvalidRgbaLength {
                expected: 4,
                observed: 8,
            }
        );
    }

    #[test]
    fn request_id_and_reserved_bytes_are_checked_before_rgba_body_read() {
        let response = |request_id: u64, reserved: u32| {
            let mut bytes = Vec::new();
            write_header(
                &mut bytes,
                Header::new(MessageKind::DecodeSuccess, DECODE_SUCCESS_PREFIX_LEN + 4).unwrap(),
            )
            .unwrap();
            let mut prefix = [0_u8; DECODE_SUCCESS_PREFIX_LEN as usize];
            prefix[..8].copy_from_slice(&request_id.to_le_bytes());
            prefix[8..12].copy_from_slice(&1_u32.to_le_bytes());
            prefix[12..16].copy_from_slice(&1_u32.to_le_bytes());
            prefix[16..20].copy_from_slice(&4_u32.to_le_bytes());
            prefix[20..24].copy_from_slice(&reserved.to_le_bytes());
            bytes.extend_from_slice(&prefix);
            bytes
        };

        assert_eq!(
            read_decode_response(&mut Cursor::new(response(8, 0)), 7).unwrap_err(),
            ProtocolError::RequestIdMismatch {
                expected: 7,
                observed: 8,
            }
        );
        assert_eq!(
            read_decode_response(&mut Cursor::new(response(7, 1)), 7).unwrap_err(),
            ProtocolError::NonCanonicalReservedBytes
        );
    }

    #[test]
    fn error_round_trip_uses_numeric_code_and_validates_request_id() {
        let error = DecodeError {
            request_id: 99,
            code: WireErrorCode::DecodeLimitExceeded,
            arg0: 600,
            arg1: 512,
        };
        let mut response = Vec::new();
        write_decode_error(&mut response, error).unwrap();
        assert_eq!(
            read_decode_response(&mut Cursor::new(response.clone()), 99).unwrap(),
            DecodeResponse::Error(error)
        );
        assert_eq!(
            read_decode_response(&mut Cursor::new(response), 100).unwrap_err(),
            ProtocolError::RequestIdMismatch {
                expected: 100,
                observed: 99,
            }
        );
    }

    #[test]
    fn success_body_length_must_match_exact_rgba_length_before_allocation() {
        let mut bytes = Vec::new();
        write_header(
            &mut bytes,
            Header::new(MessageKind::DecodeSuccess, DECODE_SUCCESS_PREFIX_LEN + 100).unwrap(),
        )
        .unwrap();
        let mut prefix = [0_u8; DECODE_SUCCESS_PREFIX_LEN as usize];
        prefix[..8].copy_from_slice(&5_u64.to_le_bytes());
        prefix[8..12].copy_from_slice(&1_u32.to_le_bytes());
        prefix[12..16].copy_from_slice(&1_u32.to_le_bytes());
        prefix[16..20].copy_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(&prefix);

        assert_eq!(
            read_decode_response(&mut Cursor::new(bytes), 5).unwrap_err(),
            ProtocolError::InvalidPayloadLength {
                kind: MessageKind::DecodeSuccess,
                observed: DECODE_SUCCESS_PREFIX_LEN + 100,
                minimum: DECODE_SUCCESS_PREFIX_LEN + 4,
                maximum: DECODE_SUCCESS_PREFIX_LEN + 4,
            }
        );
    }

    #[test]
    fn truncated_success_rgba_reports_body_length_without_accepting_partial_render() {
        let rgba = rgba(1, 1);
        let mut response = Vec::new();
        write_decode_success(&mut response, 5, 1, 1, &rgba).unwrap();
        response.truncate(response.len() - 1);
        assert_eq!(
            read_decode_response(&mut Cursor::new(response), 5).unwrap_err(),
            ProtocolError::TruncatedPayload {
                expected: rgba.len(),
                bytes_read: rgba.len() - 1,
            }
        );
    }

    #[test]
    fn invalid_dimensions_and_pixel_limit_are_rejected_before_rgba_body_read() {
        for (width, height) in [(0_u32, 1_u32), (MAX_RENDER_SIDE + 1, 1), (10_001, 10_000)] {
            let mut bytes = Vec::new();
            write_header(
                &mut bytes,
                Header::new(MessageKind::DecodeSuccess, DECODE_SUCCESS_PREFIX_LEN).unwrap(),
            )
            .unwrap();
            let mut prefix = [0_u8; DECODE_SUCCESS_PREFIX_LEN as usize];
            prefix[..8].copy_from_slice(&9_u64.to_le_bytes());
            prefix[8..12].copy_from_slice(&width.to_le_bytes());
            prefix[12..16].copy_from_slice(&height.to_le_bytes());
            bytes.extend_from_slice(&prefix);
            assert_eq!(
                read_decode_response(&mut Cursor::new(bytes), 9).unwrap_err(),
                ProtocolError::InvalidRenderDimensions { width, height }
            );
        }
    }

    #[test]
    fn shutdown_is_an_empty_fixed_command() {
        let mut bytes = Vec::new();
        write_shutdown(&mut bytes).unwrap();
        assert_eq!(
            read_helper_command(&mut Cursor::new(bytes)).unwrap(),
            HelperCommand::Shutdown
        );
    }
}
