#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};

use imgviewer_codec_protocol::{Header, MessageKind, ProtocolError, read_header, write_header};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliError {
    MissingExecutableName,
    UnexpectedArgument,
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingExecutableName => formatter.write_str("missing helper executable name"),
            Self::UnexpectedArgument => {
                formatter.write_str("codec helper does not accept command-line arguments")
            }
        }
    }
}

impl std::error::Error for CliError {}

pub fn validate_cli_arguments(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<(), CliError> {
    let mut arguments = arguments.into_iter();
    arguments.next().ok_or(CliError::MissingExecutableName)?;
    if arguments.next().is_some() {
        return Err(CliError::UnexpectedArgument);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperError {
    Protocol(ProtocolError),
    ExpectedHello(MessageKind),
    UnexpectedPayload { kind: MessageKind, payload_len: u32 },
    UnexpectedRequest(MessageKind),
    Io(io::ErrorKind),
}

impl fmt::Display for HelperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => write!(formatter, "{error}"),
            Self::ExpectedHello(kind) => {
                write!(formatter, "expected helper handshake, received {kind:?}")
            }
            Self::UnexpectedPayload { kind, payload_len } => {
                write!(
                    formatter,
                    "{kind:?} carried unexpected {payload_len}-byte payload"
                )
            }
            Self::UnexpectedRequest(kind) => {
                write!(formatter, "unexpected helper request {kind:?}")
            }
            Self::Io(kind) => write!(formatter, "codec helper I/O error: {kind:?}"),
        }
    }
}

impl std::error::Error for HelperError {}

impl From<ProtocolError> for HelperError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

pub fn serve_once(
    input: &mut (impl Read + ?Sized),
    output: &mut (impl Write + ?Sized),
) -> Result<(), HelperError> {
    let hello = read_header(input)?;
    if hello.kind() != MessageKind::Hello {
        return Err(HelperError::ExpectedHello(hello.kind()));
    }
    require_empty_payload(hello)?;
    write_header(output, Header::empty(MessageKind::Ready))?;
    output
        .flush()
        .map_err(|error| HelperError::Io(error.kind()))?;

    let request = read_header(input)?;
    require_empty_payload(request)?;
    if request.kind() != MessageKind::DecodeHeif {
        return Err(HelperError::UnexpectedRequest(request.kind()));
    }

    write_header(output, Header::empty(MessageKind::NotImplemented))?;
    output
        .flush()
        .map_err(|error| HelperError::Io(error.kind()))
}

fn require_empty_payload(header: Header) -> Result<(), HelperError> {
    if header.payload_len() != 0 {
        return Err(HelperError::UnexpectedPayload {
            kind: header.kind(),
            payload_len: header.payload_len(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use imgviewer_codec_protocol::{HEADER_LEN, PROTOCOL_VERSION};
    use std::io::Cursor;

    fn request_stream(headers: &[Header]) -> Vec<u8> {
        headers.iter().flat_map(|header| header.encode()).collect()
    }

    #[test]
    fn handshake_is_ready_and_decode_is_explicitly_not_implemented() {
        let input = request_stream(&[
            Header::empty(MessageKind::Hello),
            Header::empty(MessageKind::DecodeHeif),
        ]);
        let mut output = Vec::new();
        serve_once(&mut Cursor::new(input), &mut output).unwrap();

        assert_eq!(output.len(), HEADER_LEN * 2);
        let mut output = Cursor::new(output);
        assert_eq!(
            read_header(&mut output).unwrap(),
            Header::empty(MessageKind::Ready)
        );
        assert_eq!(
            read_header(&mut output).unwrap(),
            Header::empty(MessageKind::NotImplemented)
        );
    }

    #[test]
    fn command_before_handshake_is_rejected_without_response() {
        let input = request_stream(&[Header::empty(MessageKind::DecodeHeif)]);
        let mut output = Vec::new();
        assert_eq!(
            serve_once(&mut Cursor::new(input), &mut output).unwrap_err(),
            HelperError::ExpectedHello(MessageKind::DecodeHeif)
        );
        assert!(output.is_empty());
    }

    #[test]
    fn handshake_payload_is_rejected_without_reading_or_allocating_it() {
        let input = request_stream(&[Header::new(MessageKind::Hello, 1).unwrap()]);
        let mut output = Vec::new();
        assert_eq!(
            serve_once(&mut Cursor::new(input), &mut output).unwrap_err(),
            HelperError::UnexpectedPayload {
                kind: MessageKind::Hello,
                payload_len: 1,
            }
        );
        assert!(output.is_empty());
    }

    #[test]
    fn wrong_protocol_version_fails_before_ready_response() {
        let mut hello = Header::empty(MessageKind::Hello).encode();
        hello[8..10].copy_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes());
        let mut output = Vec::new();
        assert_eq!(
            serve_once(&mut Cursor::new(hello), &mut output).unwrap_err(),
            HelperError::Protocol(ProtocolError::UnsupportedVersion(PROTOCOL_VERSION + 1))
        );
        assert!(output.is_empty());
    }

    #[test]
    fn truncated_request_after_ready_is_a_recoverable_protocol_error() {
        let mut input = Header::empty(MessageKind::Hello).encode().to_vec();
        input.extend_from_slice(&Header::empty(MessageKind::DecodeHeif).encode()[..5]);
        let mut output = Vec::new();
        assert_eq!(
            serve_once(&mut Cursor::new(input), &mut output).unwrap_err(),
            HelperError::Protocol(ProtocolError::TruncatedHeader { bytes_read: 5 })
        );
        assert_eq!(output, Header::empty(MessageKind::Ready).encode());
    }

    #[test]
    fn helper_rejects_missing_or_extra_cli_arguments() {
        assert_eq!(
            validate_cli_arguments(Vec::<OsString>::new()).unwrap_err(),
            CliError::MissingExecutableName
        );
        validate_cli_arguments([OsString::from("imgviewer-codec-helper.exe")]).unwrap();
        assert_eq!(
            validate_cli_arguments([
                OsString::from("imgviewer-codec-helper.exe"),
                OsString::from("C:\\private\\photo.heic"),
            ])
            .unwrap_err(),
            CliError::UnexpectedArgument
        );
    }
}
