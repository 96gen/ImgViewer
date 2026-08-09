use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupportedFormat {
    Jpeg,
    Png,
    Gif,
    Tiff,
    WebP,
    Heif,
}

impl SupportedFormat {
    pub fn from_path(path: &Path) -> Option<Self> {
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
