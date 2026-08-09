#![deny(unsafe_code)]

mod decode;
mod error;
mod format;
#[cfg(all(test, feature = "heic"))]
#[allow(
    unsafe_code,
    reason = "the explicit libheif FFI test adapter owns the native pointer lifecycle"
)]
mod heif_ffi_adapter;
mod model;

pub use decode::{
    MAX_DECODE_BYTES, MAX_INPUT_BYTES, ProductionDecoder, decode_heif_file, decode_heif_file_rgba8,
    decode_tiff_file_rgba8, encode_rgba8_png, encode_rgba8_png_checked,
};
pub use error::{ViewerError, code};
pub use format::SupportedFormat;
pub use model::{DecodedRender, DecodedRgba8};
