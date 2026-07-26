use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::ViewerError;

pub(crate) const MAX_DIRECTORY_ENTRIES: usize = 100_000;
pub(crate) const MAX_CATALOG_FILES: usize = 20_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SupportedFormat {
    Jpeg,
    Png,
    Gif,
    Tiff,
    WebP,
    Heif,
}

impl SupportedFormat {
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
            Some(Self::Jpeg)
        } else if extension.eq_ignore_ascii_case("png") {
            Some(Self::Png)
        } else if extension.eq_ignore_ascii_case("gif") {
            Some(Self::Gif)
        } else if extension.eq_ignore_ascii_case("tif") || extension.eq_ignore_ascii_case("tiff") {
            Some(Self::Tiff)
        } else if extension.eq_ignore_ascii_case("webp") {
            Some(Self::WebP)
        } else if extension.eq_ignore_ascii_case("heic") || extension.eq_ignore_ascii_case("heif") {
            Some(Self::Heif)
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub(crate) struct Catalog {
    pub files: Vec<PathBuf>,
    pub index: usize,
}

pub(crate) fn build_catalog(input: &Path) -> Result<Catalog, ViewerError> {
    let target = absolute_path(input)?;
    validate_source_path(&target)?;
    if SupportedFormat::from_path(&target).is_none() {
        return Err(ViewerError::new(
            "unsupported_extension",
            "不支援這個圖片副檔名。",
        ));
    }

    let metadata = fs::metadata(&target)
        .map_err(|error| ViewerError::invalid_path(format!("無法開啟指定圖片：{error}")))?;
    if !metadata.is_file() {
        return Err(ViewerError::invalid_path("指定路徑不是圖片檔案。"));
    }

    let parent = target
        .parent()
        .ok_or_else(|| ViewerError::invalid_path("指定圖片沒有可讀取的資料夾。"))?;
    let entries = fs::read_dir(parent)
        .map_err(|error| ViewerError::io(format!("無法列出圖片資料夾：{error}")))?;

    let mut files = Vec::new();
    let mut visited = 0_usize;
    for entry in entries {
        visited = checked_count(
            visited,
            MAX_DIRECTORY_ENTRIES,
            "directory_entry_limit_exceeded",
            "資料夾項目超過安全列舉上限。",
            "maxEntries",
        )?;
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if SupportedFormat::from_path(&path).is_none() {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_file() && !is_reparse_point(&metadata) {
            checked_count(
                files.len(),
                MAX_CATALOG_FILES,
                "catalog_file_limit_exceeded",
                "圖片清單超過安全檔案數量上限。",
                "maxFiles",
            )?;
            files.push(path);
        }
    }

    files.sort_by(|left, right| natural_path_compare(left, right));
    let index = files
        .iter()
        .position(|candidate| paths_equal(candidate, &target))
        .ok_or_else(|| ViewerError::invalid_path("指定圖片不在可讀取的圖片清單中。"))?;

    Ok(Catalog { files, index })
}

pub(crate) fn validate_source_path(path: &Path) -> Result<(), ViewerError> {
    ensure_local_disk_path(path)?;
    ensure_no_reparse_components(path)
}

fn checked_count(
    current: usize,
    limit: usize,
    code: &'static str,
    message: &'static str,
    parameter: &'static str,
) -> Result<usize, ViewerError> {
    let next = current.saturating_add(1);
    if next > limit {
        return Err(ViewerError::limit(code, message).with_parameter(parameter, limit as u64));
    }
    Ok(next)
}

#[cfg(windows)]
fn ensure_local_disk_path(path: &Path) -> Result<(), ViewerError> {
    use std::path::{Component, Prefix};

    let drive_letter = match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => Some(letter),
            _ => None,
        },
        _ => None,
    };
    let Some(drive_letter) = drive_letter.filter(u8::is_ascii_alphabetic) else {
        return Err(ViewerError::new(
            "network_path_not_allowed",
            "基於離線與隱私政策，不允許 UNC、網路或裝置路徑。",
        ));
    };
    let drive_type = windows_drive_policy::query(drive_letter);
    if !windows_drive_policy::is_allowed(drive_type) {
        return Err(ViewerError::new(
            "network_path_not_allowed",
            "基於離線與隱私政策，不允許 UNC、網路或裝置路徑。",
        )
        .with_parameter("driveType", u64::from(drive_type)));
    }
    Ok(())
}

#[cfg(not(windows))]
fn ensure_local_disk_path(_path: &Path) -> Result<(), ViewerError> {
    Ok(())
}

#[cfg(windows)]
mod windows_drive_policy {
    #![allow(unsafe_code)]

    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

    #[cfg(test)]
    const DRIVE_UNKNOWN: u32 = 0;
    #[cfg(test)]
    const DRIVE_NO_ROOT_DIR: u32 = 1;
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const DRIVE_RAMDISK: u32 = 6;

    pub(super) fn query(drive_letter: u8) -> u32 {
        let root = [
            u16::from(drive_letter.to_ascii_uppercase()),
            u16::from(b':'),
            u16::from(b'\\'),
            0,
        ];
        // SAFETY: `root` is a valid NUL-terminated `X:\` UTF-16 buffer and
        // remains alive and immutable for this synchronous Win32 call.
        unsafe { GetDriveTypeW(root.as_ptr()) }
    }

    pub(super) fn is_allowed(drive_type: u32) -> bool {
        drive_type != DRIVE_REMOTE
            && matches!(
                drive_type,
                DRIVE_REMOVABLE | DRIVE_FIXED | DRIVE_CDROM | DRIVE_RAMDISK
            )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn drive_type_decision_allows_only_local_backed_storage() {
            for drive_type in [DRIVE_REMOVABLE, DRIVE_FIXED, DRIVE_CDROM, DRIVE_RAMDISK] {
                assert!(is_allowed(drive_type), "{drive_type}");
            }
            for drive_type in [DRIVE_UNKNOWN, DRIVE_NO_ROOT_DIR, DRIVE_REMOTE, u32::MAX] {
                assert!(!is_allowed(drive_type), "{drive_type}");
            }
        }
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn ensure_no_reparse_components(path: &Path) -> Result<(), ViewerError> {
    let ancestors: Vec<_> = path.ancestors().collect();
    // Inspect root-to-leaf. Checking the final file first could already follow
    // a replaced parent junction (including to remote storage) before the
    // policy notices that the parent itself is a reparse point.
    for component in ancestors.into_iter().rev() {
        let metadata = fs::symlink_metadata(component)
            .map_err(|error| ViewerError::invalid_path(format!("無法檢查指定圖片路徑：{error}")))?;
        if is_reparse_point(&metadata) {
            return Err(ViewerError::new(
                "reparse_point_not_allowed",
                "基於離線與路徑一致性政策，不允許 reparse point 或符號連結。",
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn paths_equal(left: &Path, right: &Path) -> bool {
    windows_path_identity::equal(left.as_os_str(), right.as_os_str())
}

#[cfg(not(windows))]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(windows)]
mod windows_path_identity {
    #![allow(unsafe_code)]

    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::TRUE;
    use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};

    pub(super) fn equal(left: &OsStr, right: &OsStr) -> bool {
        let left: Vec<u16> = left.encode_wide().collect();
        let right: Vec<u16> = right.encode_wide().collect();
        let Ok(left_len) = i32::try_from(left.len()) else {
            return false;
        };
        let Ok(right_len) = i32::try_from(right.len()) else {
            return false;
        };
        // SAFETY: Both slices remain alive and immutable for the synchronous
        // call; their exact lengths are representable as i32, so Win32 never
        // reads beyond either UTF-16 buffer.
        unsafe {
            CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, TRUE)
                == CSTR_EQUAL
        }
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, ViewerError> {
    if path.as_os_str().is_empty() {
        return Err(ViewerError::invalid_path("未提供檔案路徑。"));
    }
    // `std::path::absolute` performs lexical absolute normalization (on
    // Windows through GetFullPathNameW). It resolves `.` and `..` without
    // canonicalizing or touching the filesystem, so CLI-relative paths keep
    // working without following a reparse point during catalog identity.
    std::path::absolute(path).map_err(|error| ViewerError::io(format!("無法解析檔案路徑：{error}")))
}

fn natural_path_compare(left: &Path, right: &Path) -> Ordering {
    let left = left.file_name().unwrap_or(left.as_os_str());
    let right = right.file_name().unwrap_or(right.as_os_str());
    natural_name_compare(left, right).then_with(|| left.cmp(right))
}

#[cfg(windows)]
fn natural_name_compare(left: &OsStr, right: &OsStr) -> Ordering {
    windows_natural_sort::compare(left, right)
}

#[cfg(windows)]
mod windows_natural_sort {
    #![allow(unsafe_code)]

    use super::{Ordering, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::StrCmpLogicalW;

    pub(super) fn compare(left: &OsStr, right: &OsStr) -> Ordering {
        let mut left_wide: Vec<u16> = left.encode_wide().collect();
        let mut right_wide: Vec<u16> = right.encode_wide().collect();
        left_wide.push(0);
        right_wide.push(0);
        // SAFETY: Both buffers are NUL-terminated, point to valid UTF-16
        // storage, and remain alive and immutable for the duration of the call.
        let result = unsafe { StrCmpLogicalW(left_wide.as_ptr(), right_wide.as_ptr()) };
        result.cmp(&0)
    }
}

#[cfg(not(windows))]
fn natural_name_compare(left: &OsStr, right: &OsStr) -> Ordering {
    natural_string_compare(&left.to_string_lossy(), &right.to_string_lossy())
}

#[cfg(not(windows))]
fn natural_string_compare(left: &str, right: &str) -> Ordering {
    let mut left = left.chars().peekable();
    let mut right = right.chars().peekable();
    loop {
        match (left.peek(), right.peek()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) if a.is_ascii_digit() && b.is_ascii_digit() => {
                let left_number: String = left.by_ref().take_while(char::is_ascii_digit).collect();
                let right_number: String =
                    right.by_ref().take_while(char::is_ascii_digit).collect();
                let left_trimmed = left_number.trim_start_matches('0');
                let right_trimmed = right_number.trim_start_matches('0');
                let ordering = left_trimmed
                    .len()
                    .cmp(&right_trimmed.len())
                    .then_with(|| left_trimmed.cmp(right_trimmed))
                    .then_with(|| left_number.len().cmp(&right_number.len()));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(_), Some(_)) => {
                let a = left.next().expect("peeked character").to_ascii_lowercase();
                let b = right.next().expect("peeked character").to_ascii_lowercase();
                if a != b {
                    return a.cmp(&b);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn extension_matching_is_case_insensitive() {
        for name in [
            "a.GIF", "a.JpG", "a.JPEG", "a.PNG", "a.TIF", "a.TIFF", "a.WEBP", "a.HEIC", "a.HEIF",
        ] {
            assert!(
                SupportedFormat::from_path(Path::new(name)).is_some(),
                "{name}"
            );
        }
        assert!(SupportedFormat::from_path(Path::new("a.bmp")).is_none());
        assert!(SupportedFormat::from_path(Path::new("a.jpg.exe")).is_none());
    }

    #[test]
    fn catalog_uses_windows_logical_number_order() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["10.jpg", "2.jpg", "1.jpg", "ignored.txt"] {
            File::create(directory.path().join(name)).unwrap();
        }

        let catalog = build_catalog(&directory.path().join("2.jpg")).unwrap();
        let names: Vec<_> = catalog
            .files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["1.jpg", "2.jpg", "10.jpg"]);
        assert_eq!(catalog.index, 1);
    }

    #[cfg(windows)]
    #[test]
    fn catalog_matches_windows_paths_case_insensitively_without_canonicalizing() {
        let directory = tempfile::tempdir().unwrap();
        File::create(directory.path().join("Photo.PNG")).unwrap();

        let catalog = build_catalog(&directory.path().join("photo.png")).unwrap();
        assert_eq!(catalog.index, 0);
        assert_eq!(
            catalog.files[0].file_name().unwrap().to_string_lossy(),
            "Photo.PNG"
        );
    }

    #[test]
    fn relative_parent_components_are_normalized_lexically_for_catalog_identity() {
        let current = std::env::current_dir().unwrap();
        let directory = tempfile::Builder::new()
            .prefix("imgviewer-relative-catalog-")
            .tempdir_in(&current)
            .unwrap();
        File::create(directory.path().join("photo.png")).unwrap();
        let relative_directory = directory.path().strip_prefix(&current).unwrap();
        let relative_input = relative_directory
            .join("not-created")
            .join("..")
            .join("photo.png");

        let catalog = build_catalog(&relative_input).unwrap();
        assert_eq!(catalog.index, 0);
        assert_eq!(
            catalog.files[0].file_name().unwrap().to_string_lossy(),
            "photo.png"
        );
    }

    #[test]
    fn catalog_count_limits_fail_closed_with_stable_parameters() {
        assert_eq!(
            checked_count(
                MAX_DIRECTORY_ENTRIES - 1,
                MAX_DIRECTORY_ENTRIES,
                "directory_entry_limit_exceeded",
                "limit",
                "maxEntries",
            )
            .unwrap(),
            MAX_DIRECTORY_ENTRIES
        );
        let error = checked_count(
            MAX_DIRECTORY_ENTRIES,
            MAX_DIRECTORY_ENTRIES,
            "directory_entry_limit_exceeded",
            "limit",
            "maxEntries",
        )
        .unwrap_err();
        assert_eq!(error.code, "directory_entry_limit_exceeded");
        assert_eq!(
            error.parameters["maxEntries"],
            serde_json::json!(MAX_DIRECTORY_ENTRIES)
        );
    }

    #[cfg(windows)]
    #[test]
    fn unc_and_device_paths_are_rejected_without_touching_the_network() {
        for path in [
            Path::new(r"\\server\share\image.png"),
            Path::new(r"\\?\UNC\server\share\image.png"),
            Path::new(r"\\.\C:\image.png"),
        ] {
            assert_eq!(
                ensure_local_disk_path(path).unwrap_err().code,
                "network_path_not_allowed"
            );
        }
        ensure_local_disk_path(Path::new(r"C:\images\photo.png")).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn final_component_reparse_points_are_rejected() {
        use std::os::windows::fs::symlink_file;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.png");
        let link = directory.path().join("link.png");
        File::create(&source).unwrap();
        if let Err(error) = symlink_file(&source, &link) {
            eprintln!("skipping symlink assertion without Windows privilege: {error}");
            return;
        }

        assert_eq!(
            ensure_no_reparse_components(&link).unwrap_err().code,
            "reparse_point_not_allowed"
        );
    }
}
