use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::FromRawHandle;

use imgviewer_codec_core::MAX_INPUT_BYTES;

type WinHandle = *mut c_void;

const FILE_TYPE_DISK: u32 = 1;

// SAFETY: these declarations exactly match the documented Win32 ABI. All
// calls and ownership transitions are contained in this adapter.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CloseHandle(object: WinHandle) -> i32;
    fn GetFileType(file: WinHandle) -> u32;

    #[cfg(test)]
    fn CreatePipe(
        read_pipe: *mut WinHandle,
        write_pipe: *mut WinHandle,
        pipe_attributes: *const c_void,
        size: u32,
    ) -> i32;
    #[cfg(test)]
    fn GetHandleInformation(object: WinHandle, flags: *mut u32) -> i32;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandleError {
    InvalidHandle,
    NotDisk { file_type: u32 },
    Metadata(io::ErrorKind),
    FileTooLarge { observed: u64, maximum: u64 },
    LengthMismatch { expected: u64, observed: u64 },
}

/// Takes ownership of a duplicated handle and returns the same disk file.
///
/// Zero and `INVALID_HANDLE_VALUE` are rejected before any Win32 call. After a
/// successful `GetFileType`, `File::from_raw_handle` becomes the sole owner, so
/// every later success, error, and unwind closes the handle through `Drop`.
pub(crate) fn take_disk_file(
    duplicated_handle: u64,
    expected_length: u64,
) -> Result<File, HandleError> {
    let handle_value =
        usize::try_from(duplicated_handle).map_err(|_| HandleError::InvalidHandle)?;
    if handle_value == 0 || handle_value == usize::MAX {
        return Err(HandleError::InvalidHandle);
    }
    let handle = handle_value as WinHandle;

    // SAFETY: the broker transferred this non-sentinel handle value to the
    // helper. GetFileType does not take ownership and safely reports failure
    // for a stale or otherwise invalid kernel handle.
    let file_type = unsafe { GetFileType(handle) };
    if file_type != FILE_TYPE_DISK {
        // SAFETY: ownership was transferred with the request. CloseHandle is
        // the matching Win32 release operation for both valid non-disk handles
        // and rejected stale values; failure simply means nothing was owned.
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(HandleError::NotDisk { file_type });
    }

    // SAFETY: GetFileType just verified this exact transferred value is a disk
    // handle, and no other Rust owner exists in the helper. File now owns it
    // exactly once and closes it on every return or unwind path.
    let file = unsafe { File::from_raw_handle(handle) };
    let observed_length = file
        .metadata()
        .map_err(|error| HandleError::Metadata(error.kind()))?
        .len();
    if observed_length > MAX_INPUT_BYTES || expected_length > MAX_INPUT_BYTES {
        return Err(HandleError::FileTooLarge {
            observed: observed_length.max(expected_length),
            maximum: MAX_INPUT_BYTES,
        });
    }
    if observed_length != expected_length {
        return Err(HandleError::LengthMismatch {
            expected: expected_length,
            observed: observed_length,
        });
    }
    Ok(file)
}

#[cfg(test)]
pub(crate) fn handle_is_open(duplicated_handle: u64) -> bool {
    let Ok(handle_value) = usize::try_from(duplicated_handle) else {
        return false;
    };
    let handle = handle_value as WinHandle;
    let mut flags = 0_u32;
    // SAFETY: GetHandleInformation only queries the supplied numeric handle.
    // It returns zero for a closed or invalid handle and never takes ownership.
    unsafe { GetHandleInformation(handle, &mut flags) != 0 }
}

#[cfg(test)]
fn create_anonymous_pipe_read_handle() -> u64 {
    let mut read_pipe = std::ptr::null_mut();
    let mut write_pipe = std::ptr::null_mut();
    // SAFETY: both output pointers are valid for writes, attributes is null as
    // permitted by CreatePipe, and the write end is closed exactly once after
    // successful creation. The returned read end transfers to the caller.
    unsafe {
        assert_ne!(
            CreatePipe(&mut read_pipe, &mut write_pipe, std::ptr::null(), 0),
            0
        );
        assert_ne!(CloseHandle(write_pipe), 0);
    }
    read_pipe as usize as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::windows::io::IntoRawHandle;
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures")
            .join(name)
    }

    fn transferred_fixture() -> (u64, u64) {
        let file = File::open(fixture("primary-second.heic")).unwrap();
        let length = file.metadata().unwrap().len();
        let raw = file.into_raw_handle() as usize as u64;
        (raw, length)
    }

    #[test]
    fn rejects_null_and_invalid_sentinel_without_claiming_them() {
        assert_eq!(
            take_disk_file(0, 0).unwrap_err(),
            HandleError::InvalidHandle
        );
        assert_eq!(
            take_disk_file(u64::MAX, 0).unwrap_err(),
            HandleError::InvalidHandle
        );
    }

    #[test]
    fn exact_disk_handle_is_owned_and_closed_by_file_drop() {
        let (raw, length) = transferred_fixture();
        assert!(handle_is_open(raw));
        let mut file = take_disk_file(raw, length).unwrap();
        let mut signature = [0_u8; 12];
        file.read_exact(&mut signature).unwrap();
        assert_eq!(&signature[4..8], b"ftyp");
        drop(file);
        assert!(!handle_is_open(raw));
    }

    #[test]
    fn length_mismatch_and_limit_errors_close_transferred_handle() {
        let (mismatch_raw, length) = transferred_fixture();
        assert_eq!(
            take_disk_file(mismatch_raw, length + 1).unwrap_err(),
            HandleError::LengthMismatch {
                expected: length + 1,
                observed: length,
            }
        );
        assert!(!handle_is_open(mismatch_raw));

        let (oversize_raw, _) = transferred_fixture();
        assert_eq!(
            take_disk_file(oversize_raw, MAX_INPUT_BYTES + 1).unwrap_err(),
            HandleError::FileTooLarge {
                observed: MAX_INPUT_BYTES + 1,
                maximum: MAX_INPUT_BYTES,
            }
        );
        assert!(!handle_is_open(oversize_raw));
    }

    #[test]
    fn non_disk_handle_is_rejected_and_closed() {
        let raw = create_anonymous_pipe_read_handle();
        assert!(handle_is_open(raw));
        assert_eq!(
            take_disk_file(raw, 0).unwrap_err(),
            HandleError::NotDisk { file_type: 3 }
        );
        assert!(!handle_is_open(raw));
    }
}
