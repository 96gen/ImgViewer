use std::fs::{File, OpenOptions};
use std::path::Path;

use crate::catalog::validate_source_path;
use crate::error::{ViewerError, code as error_code};

pub(crate) use imgviewer_codec_core::{MAX_DECODE_BYTES, ProductionDecoder};

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
