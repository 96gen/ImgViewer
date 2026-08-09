use std::fs::{File, OpenOptions};
use std::path::Path;

use crate::catalog::validate_source_path;
use crate::codec_helper::CodecHelperClient;
use crate::error::{ViewerError, code as error_code};

pub(crate) use imgviewer_codec_core::MAX_DECODE_BYTES;
use imgviewer_codec_core::SupportedFormat;
use imgviewer_codec_protocol::CodecFormat;

#[derive(Default)]
pub(crate) struct ProductionDecoder {
    local: imgviewer_codec_core::ProductionDecoder,
    helper: CodecHelperClient,
}

impl ProductionDecoder {
    pub(crate) fn decode(
        &self,
        path: &Path,
        file: File,
    ) -> Result<imgviewer_codec_core::DecodedRender, ViewerError> {
        match helper_format(path) {
            Some(format) => self.helper.decode(format, file),
            None => self.local.decode(path, file),
        }
    }

    pub(crate) fn cancel_current(&self) {
        self.helper.cancel_current();
    }

    pub(crate) fn shutdown(&self) {
        self.helper.shutdown();
    }
}

fn helper_format(path: &Path) -> Option<CodecFormat> {
    match SupportedFormat::from_path(path) {
        Some(SupportedFormat::Heif) => Some(CodecFormat::Heif),
        Some(SupportedFormat::Tiff) => Some(CodecFormat::Tiff),
        _ => None,
    }
}

#[cfg(windows)]
pub(crate) fn open_read_only(path: &Path) -> Result<File, ViewerError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    // The catalog is only a snapshot. Re-run the drive and every-component
    // reparse policy immediately before CreateFile so navigation cannot rely
    // on stale directory identity.
    validate_source_path_for_open(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| ViewerError::io(format!("無法讀取檔案：{error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| ViewerError::io(format!("無法檢查檔案屬性：{error}")))?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ViewerError::new(
            "reparse_point_not_allowed",
            "基於離線與路徑一致性政策，不允許 reparse point 或符號連結。",
        ));
    }
    Ok(file)
}

#[cfg(not(windows))]
pub(crate) fn open_read_only(path: &Path) -> Result<File, ViewerError> {
    validate_source_path_for_open(path)?;
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| ViewerError::io(format!("無法讀取檔案：{error}")))
}

fn validate_source_path_for_open(path: &Path) -> Result<(), ViewerError> {
    validate_source_path(path).map_err(|error| {
        if error.code == error_code::INVALID_PATH {
            // Preserve the established navigation contract: a catalog entry
            // deleted before open is a recoverable I/O failure. Security
            // policy errors such as reparse/remote-drive rejection retain
            // their distinct stable codes.
            ViewerError::io(error.message)
        } else {
            error
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heif_and_tiff_are_routed_to_the_codec_helper() {
        assert_eq!(
            helper_format(Path::new("image.heic")),
            Some(CodecFormat::Heif)
        );
        assert_eq!(
            helper_format(Path::new("image.HEIF")),
            Some(CodecFormat::Heif)
        );
        assert_eq!(
            helper_format(Path::new("image.tif")),
            Some(CodecFormat::Tiff)
        );
        assert_eq!(
            helper_format(Path::new("image.TIFF")),
            Some(CodecFormat::Tiff)
        );
        assert_eq!(helper_format(Path::new("image.png")), None);
    }
}
