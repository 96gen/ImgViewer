#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};

use imgviewer_codec_protocol::{
    DecodeError, HelperCommand, ProtocolError, WireErrorCode, read_hello, read_helper_command,
    write_decode_error, write_ready,
};

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
    Io(io::ErrorKind),
}

impl fmt::Display for HelperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => write!(formatter, "{error}"),
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
    read_hello(input)?;
    write_ready(output)?;
    output
        .flush()
        .map_err(|error| HelperError::Io(error.kind()))?;

    match read_helper_command(input)? {
        HelperCommand::DecodeHeif(request) => {
            write_decode_error(
                output,
                DecodeError {
                    request_id: request.request_id,
                    code: WireErrorCode::NotImplemented,
                    arg0: 0,
                    arg1: 0,
                },
            )?;
            output
                .flush()
                .map_err(|error| HelperError::Io(error.kind()))
        }
        HelperCommand::Shutdown => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imgviewer_codec_protocol::{
        DecodeHeifRequest, DecodeResponse, Header, MessageKind, PROTOCOL_VERSION, ProtocolError,
        read_decode_response, read_ready, write_decode_request, write_hello, write_shutdown,
    };
    use std::io::Cursor;

    #[test]
    fn handshake_is_ready_and_scaffold_returns_numeric_not_implemented() {
        let request = DecodeHeifRequest {
            request_id: 17,
            duplicated_handle: 0x1234,
            expected_length: 40,
        };
        let mut input = Vec::new();
        write_hello(&mut input).unwrap();
        write_decode_request(&mut input, request).unwrap();
        let mut output = Vec::new();
        serve_once(&mut Cursor::new(input), &mut output).unwrap();

        let mut output = Cursor::new(output);
        read_ready(&mut output).unwrap();
        assert_eq!(
            read_decode_response(&mut output, request.request_id).unwrap(),
            DecodeResponse::Error(DecodeError {
                request_id: request.request_id,
                code: WireErrorCode::NotImplemented,
                arg0: 0,
                arg1: 0,
            })
        );
    }

    #[test]
    fn command_before_handshake_is_rejected_without_response() {
        let mut input = Vec::new();
        write_decode_request(
            &mut input,
            DecodeHeifRequest {
                request_id: 1,
                duplicated_handle: 2,
                expected_length: 3,
            },
        )
        .unwrap();
        let mut output = Vec::new();
        assert_eq!(
            serve_once(&mut Cursor::new(input), &mut output).unwrap_err(),
            HelperError::Protocol(ProtocolError::UnexpectedMessage {
                observed: MessageKind::DecodeHeif,
            })
        );
        assert!(output.is_empty());
    }

    #[test]
    fn handshake_payload_is_rejected_without_reading_or_allocating_it() {
        let mut hello = Header::hello().encode().to_vec();
        hello[12..16].copy_from_slice(&1_u32.to_le_bytes());
        let mut output = Vec::new();
        assert_eq!(
            serve_once(&mut Cursor::new(hello), &mut output).unwrap_err(),
            HelperError::Protocol(ProtocolError::InvalidPayloadLength {
                kind: MessageKind::Hello,
                observed: 1,
                minimum: 0,
                maximum: 0,
            })
        );
        assert!(output.is_empty());
    }

    #[test]
    fn wrong_protocol_version_fails_before_ready_response() {
        let mut hello = Header::hello().encode();
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
        let mut input = Vec::new();
        write_hello(&mut input).unwrap();
        let request_header = Header::new(
            MessageKind::DecodeHeif,
            imgviewer_codec_protocol::DECODE_REQUEST_LEN,
        )
        .unwrap();
        input.extend_from_slice(&request_header.encode());
        input.extend_from_slice(&[0_u8; 5]);
        let mut output = Vec::new();
        assert_eq!(
            serve_once(&mut Cursor::new(input), &mut output).unwrap_err(),
            HelperError::Protocol(ProtocolError::TruncatedPayload {
                expected: imgviewer_codec_protocol::DECODE_REQUEST_LEN as usize,
                bytes_read: 5,
            })
        );
        assert_eq!(output, Header::ready().encode());
    }

    #[test]
    fn shutdown_after_handshake_exits_without_a_response_body() {
        let mut input = Vec::new();
        write_hello(&mut input).unwrap();
        write_shutdown(&mut input).unwrap();
        let mut output = Vec::new();
        serve_once(&mut Cursor::new(input), &mut output).unwrap();
        assert_eq!(output, Header::ready().encode());
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
