use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::ViewerError;

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
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if SupportedFormat::from_path(&path).is_none() {
            continue;
        }
        if entry.metadata().map(|item| item.is_file()).unwrap_or(false) {
            files.push(path);
        }
    }

    files.sort_by(|left, right| natural_path_compare(left, right));
    let target_canonical = fs::canonicalize(&target).ok();
    let index = files
        .iter()
        .position(|candidate| {
            candidate == &target
                || target_canonical.as_ref().is_some_and(|canonical| {
                    fs::canonicalize(candidate)
                        .map(|candidate| &candidate == canonical)
                        .unwrap_or(false)
                })
        })
        .ok_or_else(|| ViewerError::invalid_path("指定圖片不在可讀取的圖片清單中。"))?;

    Ok(Catalog { files, index })
}

fn absolute_path(path: &Path) -> Result<PathBuf, ViewerError> {
    if path.as_os_str().is_empty() {
        return Err(ViewerError::invalid_path("未提供檔案路徑。"));
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| ViewerError::io(format!("無法解析檔案路徑：{error}")))
    }
}

fn natural_path_compare(left: &Path, right: &Path) -> Ordering {
    let left = left.file_name().unwrap_or(left.as_os_str());
    let right = right.file_name().unwrap_or(right.as_os_str());
    natural_name_compare(left, right).then_with(|| left.cmp(right))
}

#[cfg(windows)]
fn natural_name_compare(left: &OsStr, right: &OsStr) -> Ordering {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::StrCmpLogicalW;

    let mut left_wide: Vec<u16> = left.encode_wide().collect();
    let mut right_wide: Vec<u16> = right.encode_wide().collect();
    left_wide.push(0);
    right_wide.push(0);
    // SAFETY: Both buffers are NUL-terminated and remain alive for the call.
    let result = unsafe { StrCmpLogicalW(left_wide.as_ptr(), right_wide.as_ptr()) };
    result.cmp(&0)
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
}
