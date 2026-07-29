#![deny(unsafe_code)]

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "the explicit Windows handle adapter owns all transferred raw handles"
)]
mod windows_handle;

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use imgviewer_codec_core::{
    DecodedRgba8, MAX_DECODE_BYTES, MAX_INPUT_BYTES, ViewerError, code as viewer_error_code,
    decode_heif_file_rgba8,
};
use imgviewer_codec_protocol::{
    DecodeError, DecodeHeifRequest, HelperCommand, ProtocolError, WireErrorCode, read_hello,
    read_helper_command, write_decode_error, write_decode_success, write_ready,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WireFailure {
    code: WireErrorCode,
    arg0: u64,
    arg1: u64,
}

impl WireFailure {
    const fn new(code: WireErrorCode, arg0: u64, arg1: u64) -> Self {
        Self { code, arg0, arg1 }
    }

    const fn internal_decoder_error() -> Self {
        Self::new(WireErrorCode::InternalDecoderError, 0, 0)
    }

    const fn into_decode_error(self, request_id: u64) -> DecodeError {
        DecodeError {
            request_id,
            code: self.code,
            arg0: self.arg0,
            arg1: self.arg1,
        }
    }
}

impl From<ViewerError> for WireFailure {
    fn from(error: ViewerError) -> Self {
        let parameter = |name| {
            error
                .parameters
                .get(name)
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
        };
        let parameter_or = |name, fallback| {
            let value = parameter(name);
            if value == 0 { fallback } else { value }
        };
        match error.code.as_str() {
            viewer_error_code::CORRUPT_IMAGE => Self::new(WireErrorCode::CorruptImage, 0, 0),
            "format_mismatch" => Self::new(WireErrorCode::FormatMismatch, 0, 0),
            "file_too_large" => Self::new(
                WireErrorCode::FileTooLarge,
                parameter("observedBytes"),
                parameter_or("maxBytes", MAX_INPUT_BYTES),
            ),
            "dimensions_exceeded" => Self::new(
                WireErrorCode::DimensionsExceeded,
                parameter("width"),
                parameter("height"),
            ),
            "decode_limit_exceeded" => Self::new(
                WireErrorCode::DecodeLimitExceeded,
                parameter("observedBytes").max(parameter("estimatedBytes")),
                parameter_or("maxBytes", MAX_DECODE_BYTES),
            ),
            "unsupported_bit_depth" => {
                Self::new(WireErrorCode::UnsupportedBitDepth, parameter("bitDepth"), 0)
            }
            "unsupported_color_profile" | "color_profile_too_large" => Self::new(
                WireErrorCode::UnsupportedColorProfile,
                parameter("observedBytes"),
                parameter("maxBytes"),
            ),
            viewer_error_code::IO_ERROR => Self::new(WireErrorCode::IoError, 0, 0),
            "heic_unavailable" => Self::new(WireErrorCode::NotImplemented, 0, 0),
            viewer_error_code::DECODER_PANIC => Self::internal_decoder_error(),
            _ => Self::internal_decoder_error(),
        }
    }
}

#[cfg(windows)]
impl From<windows_handle::HandleError> for WireFailure {
    fn from(error: windows_handle::HandleError) -> Self {
        match error {
            windows_handle::HandleError::InvalidHandle => {
                Self::new(WireErrorCode::InvalidHandle, 0, 0)
            }
            windows_handle::HandleError::NotDisk { file_type } => {
                Self::new(WireErrorCode::InvalidHandle, u64::from(file_type), 1)
            }
            windows_handle::HandleError::Metadata(_kind) => Self::new(WireErrorCode::IoError, 0, 0),
            windows_handle::HandleError::FileTooLarge { observed, maximum } => {
                Self::new(WireErrorCode::FileTooLarge, observed, maximum)
            }
            windows_handle::HandleError::LengthMismatch { expected, observed } => {
                Self::new(WireErrorCode::InvalidHandle, expected, observed)
            }
        }
    }
}

#[cfg(windows)]
fn decode_request(request: DecodeHeifRequest) -> Result<DecodedRgba8, WireFailure> {
    let file = windows_handle::take_disk_file(request.duplicated_handle, request.expected_length)?;
    decode_heif_file_rgba8(file).map_err(WireFailure::from)
}

#[cfg(not(windows))]
fn decode_request(_request: DecodeHeifRequest) -> Result<DecodedRgba8, WireFailure> {
    Err(WireFailure::new(WireErrorCode::InvalidHandle, 0, 0))
}

fn catch_decoder(
    operation: impl FnOnce() -> Result<DecodedRgba8, WireFailure>,
) -> Result<DecodedRgba8, WireFailure> {
    catch_unwind(AssertUnwindSafe(operation))
        .unwrap_or_else(|_| Err(WireFailure::internal_decoder_error()))
}

fn write_decode_result(
    output: &mut (impl Write + ?Sized),
    request_id: u64,
    result: Result<DecodedRgba8, WireFailure>,
) -> Result<(), HelperError> {
    match result {
        Ok(render) => {
            write_decode_success(
                output,
                request_id,
                render.width,
                render.height,
                &render.rgba,
            )?;
        }
        Err(error) => {
            write_decode_error(output, error.into_decode_error(request_id))?;
        }
    }
    output
        .flush()
        .map_err(|error| HelperError::Io(error.kind()))
}

/// Serves one authenticated helper session until an explicit Shutdown command.
///
/// The handshake occurs once; DecodeHeif may then be sent repeatedly. A decode
/// failure is a numeric response and does not terminate the session.
pub fn serve(
    input: &mut (impl Read + ?Sized),
    output: &mut (impl Write + ?Sized),
) -> Result<(), HelperError> {
    read_hello(input)?;
    write_ready(output)?;
    output
        .flush()
        .map_err(|error| HelperError::Io(error.kind()))?;

    loop {
        match read_helper_command(input)? {
            HelperCommand::DecodeHeif(request) => {
                let result = catch_decoder(|| decode_request(request));
                write_decode_result(output, request.request_id, result)?;
            }
            HelperCommand::Shutdown => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imgviewer_codec_protocol::{
        DecodeResponse, Header, MessageKind, PROTOCOL_VERSION, ProtocolError, read_decode_response,
        read_ready, write_decode_request, write_hello, write_shutdown,
    };
    use std::io::Cursor;

    fn invalid_request(request_id: u64) -> DecodeHeifRequest {
        DecodeHeifRequest {
            request_id,
            duplicated_handle: 0,
            expected_length: 0,
        }
    }

    #[test]
    fn handshake_serves_multiple_decode_requests_until_shutdown() {
        let mut input = Vec::new();
        write_hello(&mut input).unwrap();
        write_decode_request(&mut input, invalid_request(17)).unwrap();
        write_decode_request(&mut input, invalid_request(18)).unwrap();
        write_shutdown(&mut input).unwrap();

        let mut output = Vec::new();
        serve(&mut Cursor::new(input), &mut output).unwrap();

        let mut output = Cursor::new(output);
        read_ready(&mut output).unwrap();
        for request_id in [17, 18] {
            assert_eq!(
                read_decode_response(&mut output, request_id).unwrap(),
                DecodeResponse::Error(DecodeError {
                    request_id,
                    code: WireErrorCode::InvalidHandle,
                    arg0: 0,
                    arg1: 0,
                })
            );
        }
        assert_eq!(output.position() as usize, output.get_ref().len());
    }

    #[test]
    fn command_before_handshake_is_rejected_without_response() {
        let mut input = Vec::new();
        write_decode_request(&mut input, invalid_request(1)).unwrap();
        let mut output = Vec::new();
        assert_eq!(
            serve(&mut Cursor::new(input), &mut output).unwrap_err(),
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
            serve(&mut Cursor::new(hello), &mut output).unwrap_err(),
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
            serve(&mut Cursor::new(hello), &mut output).unwrap_err(),
            HelperError::Protocol(ProtocolError::UnsupportedVersion(PROTOCOL_VERSION + 1))
        );
        assert!(output.is_empty());
    }

    #[test]
    fn truncated_request_after_ready_is_a_protocol_error() {
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
            serve(&mut Cursor::new(input), &mut output).unwrap_err(),
            HelperError::Protocol(ProtocolError::TruncatedPayload {
                expected: imgviewer_codec_protocol::DECODE_REQUEST_LEN as usize,
                bytes_read: 5,
            })
        );
        assert_eq!(output, Header::ready().encode());
    }

    #[test]
    fn shutdown_after_handshake_exits_without_response_body() {
        let mut input = Vec::new();
        write_hello(&mut input).unwrap();
        write_shutdown(&mut input).unwrap();
        let mut output = Vec::new();
        serve(&mut Cursor::new(input), &mut output).unwrap();
        assert_eq!(output, Header::ready().encode());
    }

    #[test]
    fn viewer_errors_map_to_numeric_fields_without_private_text() {
        let private = ViewerError::corrupt(
            "C:\\Users\\private\\secret.heic at 0x1234 contains confidential bytes",
        );
        assert_eq!(
            WireFailure::from(private),
            WireFailure::new(WireErrorCode::CorruptImage, 0, 0)
        );

        let limited = ViewerError::limit("decode_limit_exceeded", "private details")
            .with_parameter("estimatedBytes", 600_u64)
            .with_parameter("maxBytes", MAX_DECODE_BYTES);
        assert_eq!(
            WireFailure::from(limited),
            WireFailure::new(WireErrorCode::DecodeLimitExceeded, 600, MAX_DECODE_BYTES)
        );

        let dimensions = ViewerError::limit("dimensions_exceeded", "private details")
            .with_parameter("width", 32_769_u64)
            .with_parameter("height", 7_u64)
            .with_parameter("maxSide", 32_768_u64);
        assert_eq!(
            WireFailure::from(dimensions),
            WireFailure::new(WireErrorCode::DimensionsExceeded, 32_769, 7)
        );
    }

    #[test]
    fn decoder_panic_is_caught_as_internal_numeric_error() {
        let result = catch_decoder(|| -> Result<DecodedRgba8, WireFailure> {
            panic!("native decoder panic with private details")
        });
        assert_eq!(result.unwrap_err(), WireFailure::internal_decoder_error());
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

    #[cfg(windows)]
    fn transferred_fixture_request(
        request_id: u64,
    ) -> (DecodeHeifRequest, tempfile::TempDir, std::path::PathBuf) {
        use std::fs::{self, OpenOptions};
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::IntoRawHandle;
        use std::path::Path;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures")
            .join("primary-second.heic");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("transferred.heic");
        fs::copy(fixture, &path).unwrap();
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
            .unwrap();
        let expected_length = file.metadata().unwrap().len();
        let duplicated_handle = file.into_raw_handle() as usize as u64;
        (
            DecodeHeifRequest {
                request_id,
                duplicated_handle,
                expected_length,
            },
            directory,
            path,
        )
    }

    #[cfg(all(windows, not(feature = "heic")))]
    #[test]
    fn valid_owned_handle_reports_not_implemented_without_heic_feature_and_closes() {
        let (request, _directory, path) = transferred_fixture_request(31);
        let mut input = Vec::new();
        write_hello(&mut input).unwrap();
        write_decode_request(&mut input, request).unwrap();
        write_shutdown(&mut input).unwrap();
        let mut output = Vec::new();
        serve(&mut Cursor::new(input), &mut output).unwrap();

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
        std::fs::remove_file(path).expect("helper must release the transferred file handle");
    }

    #[cfg(all(windows, feature = "heic"))]
    #[test]
    fn valid_owned_handle_decodes_heif_fixture_and_closes() {
        let (request, _directory, path) = transferred_fixture_request(32);
        let mut input = Vec::new();
        write_hello(&mut input).unwrap();
        write_decode_request(&mut input, request).unwrap();
        write_shutdown(&mut input).unwrap();
        let mut output = Vec::new();
        serve(&mut Cursor::new(input), &mut output).unwrap();

        let mut output = Cursor::new(output);
        read_ready(&mut output).unwrap();
        let DecodeResponse::Success(success) =
            read_decode_response(&mut output, request.request_id).unwrap()
        else {
            panic!("expected HEIF fixture decode success");
        };
        assert_eq!((success.width, success.height), (3, 5));
        assert_eq!(success.rgba.len(), 3 * 5 * 4);
        let primary_pixel = &success.rgba[..4];
        assert!(
            primary_pixel[2] > 150
                && primary_pixel[2] > primary_pixel[0]
                && primary_pixel[2] > primary_pixel[1],
            "expected the blue designated primary item, got {primary_pixel:?}"
        );
        std::fs::remove_file(path).expect("helper must release the transferred file handle");
    }
}
