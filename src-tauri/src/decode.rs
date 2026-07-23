use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;
use std::sync::Arc;

use image::{
    ColorType, DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits,
    metadata::Orientation,
};
use moxcms::{
    Chromaticity, CicpColorPrimaries, CicpProfile, ColorPrimaries, ColorProfile, DataColorSpace,
    Layout, MatrixCoefficients, ParsingOptions, ToneReprCurve, TransferCharacteristics,
    Transform8BitExecutor, Transform16BitExecutor, XyY, curve_from_gamma,
};

use crate::catalog::SupportedFormat;
use crate::model::{DecodedRender, ViewerError};

pub(crate) const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_SIDE: u32 = 32_768;
pub(crate) const MAX_PIXELS: u64 = 100_000_000;
pub(crate) const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct ProductionDecoder;

impl ProductionDecoder {
    pub(crate) fn decode(&self, path: &Path) -> Result<DecodedRender, ViewerError> {
        decode_file(path)
    }
}

pub(crate) fn decode_file(path: &Path) -> Result<DecodedRender, ViewerError> {
    let expected = SupportedFormat::from_path(path)
        .ok_or_else(|| ViewerError::new("unsupported_extension", "不支援這個檔案的副檔名。"))?;
    let bytes = read_limited(path)?;
    let detected = sniff_format(&bytes).ok_or_else(|| {
        ViewerError::new(
            "format_mismatch",
            "檔案內容與支援的格式不符，可能已損毀或被偽裝。",
        )
    })?;
    if detected != expected {
        return Err(ViewerError::new(
            "format_mismatch",
            "檔案內容與副檔名不一致。",
        ));
    }

    match detected {
        SupportedFormat::Jpeg => preserve_raster(bytes, ImageFormat::Jpeg, "image/jpeg", false),
        SupportedFormat::Png => decode_png(bytes),
        SupportedFormat::Gif => {
            let animated = gif_is_animated(&bytes);
            preserve_raster(bytes, ImageFormat::Gif, "image/gif", animated)
        }
        SupportedFormat::WebP => {
            let animated = webp_is_animated(&bytes);
            preserve_raster(bytes, ImageFormat::WebP, "image/webp", animated)
        }
        SupportedFormat::Tiff => decode_tiff(bytes),
        SupportedFormat::Heif => decode_heif(bytes),
    }
}

fn read_limited(path: &Path) -> Result<Vec<u8>, ViewerError> {
    let file =
        File::open(path).map_err(|error| ViewerError::io(format!("無法讀取檔案：{error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| ViewerError::io(format!("無法取得檔案大小：{error}")))?;
    if metadata.len() > MAX_INPUT_BYTES {
        return Err(ViewerError::limit(
            "file_too_large",
            "檔案超過 256 MiB 上限。",
        ));
    }

    // `take` closes the TOCTOU gap if the file grows after metadata() and also
    // guarantees that an untrusted file cannot force an unbounded allocation.
    let mut reader = BufReader::new(file).take(MAX_INPUT_BYTES + 1);
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_INPUT_BYTES) as usize);
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| ViewerError::io(format!("讀取檔案時發生錯誤：{error}")))?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ViewerError::limit(
            "file_too_large",
            "檔案超過 256 MiB 上限。",
        ));
    }
    Ok(bytes)
}

pub(crate) fn sniff_format(bytes: &[u8]) -> Option<SupportedFormat> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(SupportedFormat::Jpeg)
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(SupportedFormat::Png)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(SupportedFormat::Gif)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(SupportedFormat::WebP)
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some(SupportedFormat::Tiff)
    } else if is_heif_brand(bytes) {
        Some(SupportedFormat::Heif)
    } else {
        None
    }
}

fn is_heif_brand(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let declared_size = u32::from_be_bytes(bytes[..4].try_into().expect("four bytes")) as usize;
    let box_end = declared_size.min(bytes.len());
    if declared_size < 16 || box_end < 16 {
        return false;
    }
    const BRANDS: [[u8; 4]; 8] = [
        *b"heic", *b"heix", *b"hevc", *b"hevx", *b"heim", *b"heis", *b"mif1", *b"msf1",
    ];
    BRANDS.contains(bytes[8..12].try_into().expect("four bytes"))
        || bytes[16..box_end]
            .chunks_exact(4)
            .any(|brand| BRANDS.contains(brand.try_into().expect("four-byte chunk")))
}

fn preserve_raster(
    bytes: Vec<u8>,
    format: ImageFormat,
    mime_type: &'static str,
    animated: bool,
) -> Result<DecodedRender, ViewerError> {
    let (width, height) = raster_dimensions(&bytes, format)?;
    validate_dimensions(width, height)?;

    // Read only decoder metadata for the common pass-through path. WebView2
    // owns the display decode and reports malformed scan/frame data through
    // the image error event, so eagerly materializing a second full pixel
    // plane here would only double peak RAM for ordinary images.
    let mut reader = ImageReader::with_format(Cursor::new(bytes.as_slice()), format);
    reader.limits(decode_limits());
    let mut decoder = reader.into_decoder().map_err(image_error)?;
    let color_transform = if animated {
        None
    } else {
        decoder
            .icc_profile()
            .map_err(image_error)?
            .as_deref()
            .map(|profile| rgb_icc_to_srgb_transform(profile, false))
            .transpose()?
            .flatten()
    };
    let orientation = decoder.orientation().map_err(image_error)?;
    let (display_width, display_height) = oriented_dimensions(width, height, orientation);
    validate_dimensions(display_width, display_height)?;

    if let Some(transform) = color_transform {
        // Re-open with an owned cursor only for mandatory colour conversion.
        // Consuming this decoder releases the compressed input before PNG
        // encoding starts instead of keeping both buffers alive to return it.
        drop(decoder);
        let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
        reader.limits(decode_limits());
        let decoder = reader.into_decoder().map_err(image_error)?;
        let mut decoded = DynamicImage::from_decoder(decoder).map_err(image_error)?;
        validate_dimensions(decoded.width(), decoded.height())?;
        decoded.apply_orientation(orientation);
        let mut rgba = decoded.into_rgba8();
        apply_rgb_color_transform(&mut rgba, &transform)?;
        let png = encode_rgba_png(rgba.width(), rgba.height(), rgba.as_raw())?;
        return Ok(DecodedRender {
            bytes: png,
            mime_type: "image/png",
            width: rgba.width(),
            height: rgba.height(),
            animated: false,
        });
    }
    drop(decoder);

    Ok(DecodedRender {
        bytes,
        mime_type,
        width: display_width,
        height: display_height,
        animated,
    })
}

fn decode_png(bytes: Vec<u8>) -> Result<DecodedRender, ViewerError> {
    // PNG's IHDR bit-depth byte follows width/height. Keep ordinary 8-bit
    // files byte-for-byte, but normalize 16-bit input to deterministic RGBA8
    // instead of delegating high-bit-depth display behavior to WebView2.
    let bit_depth = bytes
        .get(24)
        .copied()
        .ok_or_else(|| ViewerError::corrupt("PNG 標頭不完整，無法讀取色彩位元深度。"))?;
    let (width, height) = raster_dimensions(&bytes, ImageFormat::Png)?;
    validate_dimensions(width, height)?;
    if bit_depth > 8 {
        validate_high_bit_working_set(width, height)?;
    }
    let color_source = png_color_source(&bytes)?;
    if bit_depth <= 8 && !color_source.requires_normalization() {
        return preserve_raster(bytes, ImageFormat::Png, "image/png", false);
    }

    // The decoder owns the compressed input on normalization paths. Once the
    // DynamicImage is produced, the original bytes are freed before the RGBA
    // plane is encoded into its replacement PNG.
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    reader.limits(decode_limits());
    let mut decoder = reader.into_decoder().map_err(image_error)?;
    let source_profile = match color_source {
        PngColorSource::NativeOrEmbeddedIcc => decoder
            .icc_profile()
            .map_err(image_error)?
            .as_deref()
            .map(|profile| parse_rgb_icc_profile(profile, true))
            .transpose()?
            .flatten(),
        PngColorSource::ExplicitSrgb { .. } => None,
        PngColorSource::ExplicitProfile(profile) => Some(*profile),
    };
    let orientation = decoder.orientation().map_err(image_error)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(image_error)?;
    image.apply_orientation(orientation);
    let width = image.width();
    let height = image.height();
    validate_dimensions(width, height)?;
    let rgba = if bit_depth > 8 {
        let mut rgba16 = image.into_rgba16();
        if let Some(profile) = source_profile.as_ref() {
            apply_profile_transform_16(&mut rgba16, profile, 16)?;
        }
        quantize_rgba16(rgba16.as_raw(), 16)?
    } else {
        let mut rgba8 = image.into_rgba8();
        if let Some(profile) = source_profile.as_ref() {
            let transform = create_transform_8(profile)?;
            apply_rgb_color_transform(&mut rgba8, &transform)?;
        }
        rgba8.into_raw()
    };
    let png = encode_rgba_png(width, height, &rgba)?;
    Ok(DecodedRender {
        bytes: png,
        mime_type: "image/png",
        width,
        height,
        animated: false,
    })
}

#[derive(Debug)]
enum PngColorSource {
    NativeOrEmbeddedIcc,
    ExplicitSrgb { normalize: bool },
    ExplicitProfile(Box<ColorProfile>),
}

impl PngColorSource {
    fn requires_normalization(&self) -> bool {
        matches!(
            self,
            Self::ExplicitProfile(_) | Self::ExplicitSrgb { normalize: true }
        )
    }
}

fn png_color_source(bytes: &[u8]) -> Result<PngColorSource, ViewerError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    let limits = png::Limits {
        bytes: MAX_ICC_PROFILE_BYTES + 8 * 1024 * 1024,
    };
    decoder.set_limits(limits);
    let reader = decoder
        .read_info()
        .map_err(|error| ViewerError::corrupt(format!("PNG 色彩資訊無法解析：{error}")))?;
    let info = reader.info();

    // PNG 3 gives a supported cICP chunk highest precedence, including over
    // iCCP. Normalize even an sRGB cICP file so a WebView that does not
    // understand cICP cannot accidentally honor a conflicting lower-priority
    // profile.
    if let Some(cicp) = info.coding_independent_code_points {
        if cicp.matrix_coefficients != 0 || !cicp.is_video_full_range_image {
            return Err(ViewerError::new(
                "unsupported_color_profile",
                "PNG cICP 必須是 full-range RGB identity matrix。",
            ));
        }
        return Ok(
            match color_profile_from_cicp(cicp.color_primaries, cicp.transfer_function, true)? {
                Some(profile) => PngColorSource::ExplicitProfile(Box::new(profile)),
                None => PngColorSource::ExplicitSrgb { normalize: true },
            },
        );
    }

    // The image decoder exposes the decompressed ICC bytes later, where the
    // same 16 MiB policy and RGB validation are enforced.
    if let Some(profile) = info.icc_profile.as_deref() {
        if profile.len() > MAX_ICC_PROFILE_BYTES {
            return Err(ViewerError::limit(
                "color_profile_too_large",
                "圖片的 ICC 色彩描述超過 16 MiB 上限。",
            ));
        }
        return Ok(PngColorSource::NativeOrEmbeddedIcc);
    }

    if info.srgb.is_some() {
        return Ok(PngColorSource::ExplicitSrgb { normalize: false });
    }

    match (info.chromaticities(), info.gamma()) {
        (Some(chromaticities), Some(gamma)) => {
            let gamma = gamma.into_value();
            if !gamma.is_finite() || gamma <= 0.0 {
                return Err(ViewerError::new(
                    "unsupported_color_profile",
                    "PNG gAMA 色彩資訊無效。",
                ));
            }
            let point = |pair: (png::ScaledFloat, png::ScaledFloat)| {
                Chromaticity::new(pair.0.into_value(), pair.1.into_value())
            };
            let white = point(chromaticities.white);
            let primaries = ColorPrimaries {
                red: point(chromaticities.red),
                green: point(chromaticities.green),
                blue: point(chromaticities.blue),
            };
            if !valid_png_chromaticity(white)
                || !valid_png_chromaticity(primaries.red)
                || !valid_png_chromaticity(primaries.green)
                || !valid_png_chromaticity(primaries.blue)
            {
                return Err(ViewerError::new(
                    "unsupported_color_profile",
                    "PNG cHRM 色度座標無效。",
                ));
            }
            let mut profile = ColorProfile::new_srgb();
            profile.update_rgb_colorimetry(
                XyY::new(f64::from(white.x), f64::from(white.y), 1.0),
                primaries,
            );
            let curve = curve_from_gamma(1.0 / gamma);
            profile.red_trc = Some(curve.clone());
            profile.green_trc = Some(curve.clone());
            profile.blue_trc = Some(curve);
            if !profile.is_matrix_shaper()
                || !xyz_is_finite(&profile.red_colorant)
                || !xyz_is_finite(&profile.green_colorant)
                || !xyz_is_finite(&profile.blue_colorant)
                || !xyz_is_finite(&profile.white_point)
            {
                return Err(ViewerError::new(
                    "unsupported_color_profile",
                    "PNG cHRM/gAMA 無法建立有效的 RGB 色彩描述。",
                ));
            }
            if profile_is_structurally_srgb(&profile) {
                Ok(PngColorSource::ExplicitSrgb { normalize: false })
            } else {
                Ok(PngColorSource::ExplicitProfile(Box::new(profile)))
            }
        }
        // Incomplete legacy metadata does not uniquely define an RGB space.
        // Ordinary 8-bit PNG bytes stay native; high-bit normalization uses
        // the conventional sRGB fallback rather than inventing primaries.
        _ => Ok(PngColorSource::NativeOrEmbeddedIcc),
    }
}

fn valid_png_chromaticity(point: Chromaticity) -> bool {
    point.x.is_finite()
        && point.y.is_finite()
        && point.x >= 0.0
        && point.y > 0.0
        && point.x + point.y <= 1.0
}

fn xyz_is_finite(point: &moxcms::Xyzd) -> bool {
    point.x.is_finite() && point.y.is_finite() && point.z.is_finite()
}

fn color_profile_from_cicp(
    primaries: u8,
    transfer: u8,
    full_range: bool,
) -> Result<Option<ColorProfile>, ViewerError> {
    let primaries = CicpColorPrimaries::try_from(primaries).map_err(|error| {
        ViewerError::new(
            "unsupported_color_profile",
            format!("不支援的 CICP 色彩原色：{error}"),
        )
    })?;
    let transfer = TransferCharacteristics::try_from(transfer).map_err(|error| {
        ViewerError::new(
            "unsupported_color_profile",
            format!("不支援的 CICP 傳遞函數：{error}"),
        )
    })?;
    let profile = ColorProfile::new_from_cicp(CicpProfile {
        color_primaries: primaries,
        transfer_characteristics: transfer,
        // The decoder has already produced interleaved RGB samples.
        matrix_coefficients: MatrixCoefficients::Identity,
        full_range,
    });
    if !profile.is_matrix_shaper() {
        return Err(ViewerError::new(
            "unsupported_color_profile",
            "CICP 色彩資訊不完整，無法安全轉成 sRGB。",
        ));
    }
    Ok((!profile_is_structurally_srgb(&profile)).then_some(profile))
}

fn raster_dimensions(bytes: &[u8], format: ImageFormat) -> Result<(u32, u32), ViewerError> {
    // Parse fixed headers first where possible. This rejects declared oversized
    // images before a decoder attempts to allocate or inspect later chunks,
    // including intentionally truncated limit-test inputs.
    if format == ImageFormat::Png {
        if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR"
        {
            return Err(ViewerError::corrupt("PNG 缺少有效的 IHDR。"));
        }
        return Ok((
            u32::from_be_bytes(bytes[16..20].try_into().expect("four bytes")),
            u32::from_be_bytes(bytes[20..24].try_into().expect("four bytes")),
        ));
    }
    if format == ImageFormat::Gif {
        if bytes.len() < 10 {
            return Err(ViewerError::corrupt("GIF 標頭不完整。"));
        }
        return Ok((
            u16::from_le_bytes(bytes[6..8].try_into().expect("two bytes")) as u32,
            u16::from_le_bytes(bytes[8..10].try_into().expect("two bytes")) as u32,
        ));
    }

    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    reader.into_dimensions().map_err(image_error)
}

fn decode_tiff(bytes: Vec<u8>) -> Result<DecodedRender, ViewerError> {
    let (width, height) = raster_dimensions(&bytes, ImageFormat::Tiff)?;
    validate_dimensions(width, height)?;

    // TiffDecoder starts at the first IFD (the first page). Read its orientation
    // before consuming the decoder, then normalize pixels so the PNG does not
    // depend on browser-specific TIFF metadata handling.
    let mut decoder =
        image::codecs::tiff::TiffDecoder::new(Cursor::new(bytes)).map_err(image_error)?;
    decoder.set_limits(decode_limits()).map_err(image_error)?;
    let high_bit_depth = matches!(
        decoder.color_type(),
        ColorType::L16 | ColorType::La16 | ColorType::Rgb16 | ColorType::Rgba16
    );
    if high_bit_depth {
        validate_high_bit_working_set(width, height)?;
    }
    let source_profile = decoder
        .icc_profile()
        .map_err(image_error)?
        .as_deref()
        .map(|profile| parse_rgb_icc_profile(profile, true))
        .transpose()?
        .flatten();
    let orientation = decoder.orientation().map_err(image_error)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(image_error)?;
    image.apply_orientation(orientation);
    let width = image.width();
    let height = image.height();
    validate_dimensions(width, height)?;
    let rgba = if high_bit_depth {
        let mut rgba16 = image.into_rgba16();
        if let Some(profile) = source_profile.as_ref() {
            apply_profile_transform_16(&mut rgba16, profile, 16)?;
        }
        quantize_rgba16(rgba16.as_raw(), 16)?
    } else {
        let mut rgba8 = image.into_rgba8();
        if let Some(profile) = source_profile.as_ref() {
            let transform = create_transform_8(profile)?;
            apply_rgb_color_transform(&mut rgba8, &transform)?;
        }
        rgba8.into_raw()
    };
    let png = encode_rgba_png(width, height, &rgba)?;
    Ok(DecodedRender {
        bytes: png,
        mime_type: "image/png",
        width,
        height,
        animated: false,
    })
}

fn rgb_icc_to_srgb_transform(
    icc_profile: &[u8],
    reject_non_rgb: bool,
) -> Result<Option<Arc<Transform8BitExecutor>>, ViewerError> {
    parse_rgb_icc_profile(icc_profile, reject_non_rgb)?
        .as_ref()
        .map(create_transform_8)
        .transpose()
}

fn parse_rgb_icc_profile(
    icc_profile: &[u8],
    reject_non_rgb: bool,
) -> Result<Option<ColorProfile>, ViewerError> {
    if icc_profile.len() > MAX_ICC_PROFILE_BYTES {
        return Err(ViewerError::limit(
            "color_profile_too_large",
            "圖片的 ICC 色彩描述超過 16 MiB 上限。",
        ));
    }
    let profile = ColorProfile::new_from_slice_with_options(
        icc_profile,
        ParsingOptions {
            max_profile_size: MAX_ICC_PROFILE_BYTES + 1,
            ..ParsingOptions::default()
        },
    )
    .map_err(|error| ViewerError::corrupt(format!("圖片的 ICC 色彩描述無法解析：{error}")))?;
    if profile.color_space != DataColorSpace::Rgb {
        if reject_non_rgb {
            return Err(ViewerError::new(
                "unsupported_color_profile",
                "這張圖片使用無法安全轉為 RGB 的 ICC 色彩描述。",
            ));
        }
        // Raw JPEG/PNG/WebP bytes can still be colour-managed by WebView2.
        // Normalized TIFF/high-bit paths instead opt into the error above so
        // that converted pixels are never incorrectly tagged as sRGB.
        return Ok(None);
    }
    Ok((!profile_is_structurally_srgb(&profile)).then_some(profile))
}

fn create_transform_8(profile: &ColorProfile) -> Result<Arc<Transform8BitExecutor>, ViewerError> {
    profile
        .create_transform_8bit(
            Layout::Rgba,
            &ColorProfile::new_srgb(),
            Layout::Rgba,
            Default::default(),
        )
        .map_err(|error| ViewerError::corrupt(format!("無法建立 ICC 到 sRGB 的色彩轉換：{error}")))
}

fn profile_is_structurally_srgb(profile: &ColorProfile) -> bool {
    let srgb = ColorProfile::new_srgb();
    profile.pcs == DataColorSpace::Xyz
        && profile.is_matrix_shaper()
        && colorant_near(&profile.red_colorant, &srgb.red_colorant)
        && colorant_near(&profile.green_colorant, &srgb.green_colorant)
        && colorant_near(&profile.blue_colorant, &srgb.blue_colorant)
        && tone_curve_near(profile.red_trc.as_ref(), srgb.red_trc.as_ref())
        && tone_curve_near(profile.green_trc.as_ref(), srgb.green_trc.as_ref())
        && tone_curve_near(profile.blue_trc.as_ref(), srgb.blue_trc.as_ref())
}

fn colorant_near(left: &moxcms::Xyzd, right: &moxcms::Xyzd) -> bool {
    const TOLERANCE: f64 = 5e-4;
    (left.x - right.x).abs() <= TOLERANCE
        && (left.y - right.y).abs() <= TOLERANCE
        && (left.z - right.z).abs() <= TOLERANCE
}

fn tone_curve_near(left: Option<&ToneReprCurve>, right: Option<&ToneReprCurve>) -> bool {
    const TOLERANCE: f32 = 5e-4;
    match (left, right) {
        (Some(ToneReprCurve::Parametric(left)), Some(ToneReprCurve::Parametric(right))) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| (left - right).abs() <= TOLERANCE)
        }
        // LUT profiles are deliberately transformed. Finite probes cannot
        // prove that an arbitrary LUT is the identity over its full domain.
        _ => false,
    }
}

fn apply_rgb_color_transform(
    rgba: &mut image::RgbaImage,
    transform: &Arc<Transform8BitExecutor>,
) -> Result<(), ViewerError> {
    let row_bytes = (rgba.width() as usize)
        .checked_mul(4)
        .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "色彩轉換列大小溢位。"))?;
    let mut source = try_vec_with_capacity(row_bytes)?;
    source.resize(row_bytes, 0);
    for row in rgba.as_mut().chunks_exact_mut(row_bytes) {
        // moxcms has an in-place fast path only for matrix profiles. A single
        // reusable bounded row copy also supports LUT profiles without a
        // second full image allocation or one allocation per scanline.
        source.copy_from_slice(row);
        transform
            .transform(&source, row)
            .map_err(|error| ViewerError::corrupt(format!("ICC 色彩轉換失敗：{error}")))?;
    }
    Ok(())
}

fn apply_profile_transform_16(
    rgba: &mut image::ImageBuffer<image::Rgba<u16>, Vec<u16>>,
    profile: &ColorProfile,
    bit_depth: u8,
) -> Result<(), ViewerError> {
    let transform = create_transform_16(profile, bit_depth)?;
    let width = rgba.width();
    apply_rgb_color_transform_16(rgba.as_mut(), width, &transform)
}

fn create_transform_16(
    profile: &ColorProfile,
    bit_depth: u8,
) -> Result<Arc<Transform16BitExecutor>, ViewerError> {
    let destination = ColorProfile::new_srgb();
    let result = match bit_depth {
        10 => profile.create_transform_10bit(
            Layout::Rgba,
            &destination,
            Layout::Rgba,
            Default::default(),
        ),
        12 => profile.create_transform_12bit(
            Layout::Rgba,
            &destination,
            Layout::Rgba,
            Default::default(),
        ),
        16 => profile.create_transform_16bit(
            Layout::Rgba,
            &destination,
            Layout::Rgba,
            Default::default(),
        ),
        _ => {
            return Err(ViewerError::new(
                "unsupported_bit_depth",
                format!("不支援 {bit_depth}-bit 色彩轉換。"),
            ));
        }
    };
    result
        .map_err(|error| ViewerError::corrupt(format!("無法建立高位元色彩到 sRGB 的轉換：{error}")))
}

fn apply_rgb_color_transform_16(
    rgba: &mut [u16],
    width: u32,
    transform: &Arc<Transform16BitExecutor>,
) -> Result<(), ViewerError> {
    let row_values = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "色彩轉換列大小溢位。"))?;
    if row_values == 0 || !rgba.len().is_multiple_of(row_values) {
        return Err(ViewerError::corrupt("高位元 RGBA 像素平面長度不正確。"));
    }
    let mut source = try_vec_with_capacity(row_values)?;
    source.resize(row_values, 0);
    for row in rgba.chunks_exact_mut(row_values) {
        source.copy_from_slice(row);
        transform
            .transform(&source, row)
            .map_err(|error| ViewerError::corrupt(format!("ICC 色彩轉換失敗：{error}")))?;
    }
    Ok(())
}

fn quantize_rgba16(rgba: &[u16], bit_depth: u8) -> Result<Vec<u8>, ViewerError> {
    let maximum = match bit_depth {
        10 => 1_u32 << 10,
        12 => 1_u32 << 12,
        16 => 1_u32 << 16,
        _ => {
            return Err(ViewerError::new(
                "unsupported_bit_depth",
                format!("不支援 {bit_depth}-bit 圖片。"),
            ));
        }
    } - 1;
    if rgba.len() as u64 > MAX_DECODE_BYTES {
        return Err(ViewerError::limit(
            "decode_limit_exceeded",
            "量化後的 RGBA 像素需要超過 512 MiB 的記憶體。",
        ));
    }
    let mut output = try_vec_with_capacity(rgba.len())?;
    output.extend(
        rgba.iter()
            .map(|value| (((u32::from(*value).min(maximum) * 255) + maximum / 2) / maximum) as u8),
    );
    Ok(output)
}

#[cfg(any(feature = "heic", test))]
fn unpremultiply_rgba8(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel[..3].fill(0);
        } else if alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

#[cfg(any(feature = "heic", test))]
fn unpremultiply_rgba16(rgba: &mut [u16], bit_depth: u8) -> Result<(), ViewerError> {
    let maximum = match bit_depth {
        10 => (1_u32 << 10) - 1,
        12 => (1_u32 << 12) - 1,
        16 => u32::from(u16::MAX),
        _ => {
            return Err(ViewerError::new(
                "unsupported_bit_depth",
                format!("不支援 {bit_depth}-bit alpha 轉換。"),
            ));
        }
    };
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]).min(maximum);
        if alpha == 0 {
            pixel[..3].fill(0);
        } else if alpha < maximum {
            for channel in &mut pixel[..3] {
                *channel = (((u32::from(*channel).min(maximum) * maximum) + alpha / 2) / alpha)
                    .min(maximum) as u16;
            }
        }
    }
    Ok(())
}

fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, ViewerError> {
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "圖片尺寸計算溢位。"))?;
    if expected > MAX_DECODE_BYTES || rgba.len() as u64 != expected {
        return Err(ViewerError::limit(
            "decode_limit_exceeded",
            "解碼圖片需要超過 512 MiB 的記憶體。",
        ));
    }

    let mut output = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut output);
    let srgb_profile = moxcms::ColorProfile::new_srgb()
        .encode()
        .map_err(|error| ViewerError::corrupt(format!("無法建立 sRGB 色彩描述：{error}")))?;
    encoder
        .set_icc_profile(srgb_profile)
        .map_err(|error| ViewerError::corrupt(format!("無法寫入 sRGB 色彩描述：{error}")))?;
    encoder
        .write_image(rgba, width, height, ColorType::Rgba8.into())
        .map_err(image_error)?;
    if output.len() as u64 > MAX_DECODE_BYTES {
        return Err(ViewerError::limit(
            "decode_limit_exceeded",
            "轉換後的圖片超過 512 MiB 上限。",
        ));
    }
    Ok(output)
}

fn decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}

fn oriented_dimensions(width: u32, height: u32, orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    }
}

pub(crate) fn validate_dimensions(width: u32, height: u32) -> Result<(), ViewerError> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "圖片尺寸計算溢位。"))?;
    if width == 0 || height == 0 {
        return Err(ViewerError::corrupt("圖片寬度或高度為零。"));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(ViewerError::limit(
            "dimensions_exceeded",
            "圖片單邊超過 32,768 像素上限。",
        ));
    }
    if pixels > MAX_PIXELS {
        return Err(ViewerError::limit(
            "dimensions_exceeded",
            "圖片超過 100,000,000 像素上限。",
        ));
    }
    if pixels.saturating_mul(4) > MAX_DECODE_BYTES {
        return Err(ViewerError::limit(
            "decode_limit_exceeded",
            "解碼圖片需要超過 512 MiB 的記憶體。",
        ));
    }
    Ok(())
}

fn validate_high_bit_working_set(width: u32, height: u32) -> Result<(), ViewerError> {
    // High-bit normalization can hold the decoded RGBA16 plane and its RGBA8
    // result at the same time. ICC conversion additionally keeps at most two
    // u16 rows (source and destination). Include all three allocations in the
    // 512 MiB budget before any attacker-controlled full plane is allocated.
    let plane_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(8 + 4))
        .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "高位元圖片尺寸計算溢位。"))?;
    let row_bytes = u64::from(width)
        .checked_mul(16)
        .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "高位元轉換列大小溢位。"))?;
    let bytes = plane_bytes
        .checked_add(row_bytes)
        .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "高位元工作集大小溢位。"))?;
    if bytes > MAX_DECODE_BYTES {
        return Err(ViewerError::limit(
            "decode_limit_exceeded",
            "高位元圖片轉換需要超過 512 MiB 的記憶體。",
        ));
    }
    Ok(())
}

fn try_vec_with_capacity<T>(capacity: usize) -> Result<Vec<T>, ViewerError> {
    let bytes = capacity
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| ViewerError::limit("decode_limit_exceeded", "像素緩衝區尺寸溢位。"))?;
    if bytes as u64 > MAX_DECODE_BYTES {
        return Err(ViewerError::limit(
            "decode_limit_exceeded",
            "像素緩衝區需要超過 512 MiB 的記憶體。",
        ));
    }
    let mut output = Vec::new();
    output.try_reserve_exact(capacity).map_err(|_| {
        ViewerError::limit(
            "decode_limit_exceeded",
            "無法在 512 MiB 解碼上限內配置像素緩衝區。",
        )
    })?;
    Ok(output)
}

fn image_error(error: image::ImageError) -> ViewerError {
    match error {
        image::ImageError::Limits(_) => {
            ViewerError::limit("decode_limit_exceeded", "圖片超過安全解碼限制。")
        }
        other => ViewerError::corrupt(format!("圖片資料無法解碼：{other}")),
    }
}

fn gif_is_animated(bytes: &[u8]) -> bool {
    if bytes.len() < 13 {
        return false;
    }
    let packed = bytes[10];
    let global_table = if packed & 0x80 != 0 {
        3usize << ((packed & 0x07) + 1)
    } else {
        0
    };
    let mut cursor = 13usize.saturating_add(global_table);
    let mut images = 0usize;
    while cursor < bytes.len() {
        match bytes[cursor] {
            0x2c => {
                images += 1;
                if images > 1 {
                    return true;
                }
                if cursor + 10 > bytes.len() {
                    return false;
                }
                let local_packed = bytes[cursor + 9];
                cursor += 10;
                if local_packed & 0x80 != 0 {
                    cursor = cursor.saturating_add(3usize << ((local_packed & 0x07) + 1));
                }
                cursor = cursor.saturating_add(1); // LZW minimum code size.
                if !skip_sub_blocks(bytes, &mut cursor) {
                    return false;
                }
            }
            0x21 => {
                cursor = cursor.saturating_add(2); // introducer and extension label.
                if !skip_sub_blocks(bytes, &mut cursor) {
                    return false;
                }
            }
            0x3b => return false,
            _ => return false,
        }
    }
    false
}

fn skip_sub_blocks(bytes: &[u8], cursor: &mut usize) -> bool {
    while *cursor < bytes.len() {
        let size = bytes[*cursor] as usize;
        *cursor += 1;
        if size == 0 {
            return true;
        }
        let Some(next) = cursor.checked_add(size) else {
            return false;
        };
        if next > bytes.len() {
            return false;
        }
        *cursor = next;
    }
    false
}

fn webp_is_animated(bytes: &[u8]) -> bool {
    if bytes.len() < 20 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return false;
    }
    let mut cursor = 12usize;
    let mut animation_header = false;
    let mut animation_frame = false;
    while cursor + 8 <= bytes.len() {
        let chunk = &bytes[cursor..cursor + 4];
        let length = u32::from_le_bytes(
            bytes[cursor + 4..cursor + 8]
                .try_into()
                .expect("four bytes"),
        ) as usize;
        let data_start = cursor + 8;
        let Some(data_end) = data_start.checked_add(length) else {
            return false;
        };
        if data_end > bytes.len() {
            return false;
        }
        animation_header |= chunk == b"ANIM";
        animation_frame |= chunk == b"ANMF";
        cursor = data_end.saturating_add(length & 1);
    }
    animation_header && animation_frame
}

#[cfg(any(feature = "heic", test))]
fn heif_nclx_fallback_codes(primaries: u8, transfer: u8) -> (u8, u8) {
    // ISO/IEC 23008-12 follows the CICP unspecified value (2). libheif's
    // documented display fallbacks are BT.709 primaries and the sRGB transfer;
    // apply them independently because real files often omit just one field.
    (
        if primaries == 2 { 1 } else { primaries },
        if transfer == 2 { 13 } else { transfer },
    )
}

#[cfg(feature = "heic")]
fn heif_source_color_profile(
    handle: &libheif_rs::ImageHandle,
) -> Result<Option<ColorProfile>, ViewerError> {
    if let Some(profile) = handle.color_profile_raw() {
        // libheif specifies ICC precedence when both ICC and NCLX are present.
        return parse_rgb_icc_profile(&profile.data, true);
    }

    let Some(nclx) = handle.color_profile_nclx() else {
        // Images without an explicit profile use the conventional sRGB
        // display fallback after libheif has produced RGB samples.
        return Ok(None);
    };
    use libheif_rs::{
        ColorPrimaries as HeifPrimaries, MatrixCoefficients as HeifMatrix,
        TransferCharacteristics as HeifTransfer,
    };
    let primaries = nclx.color_primaries();
    let transfer = nclx.transfer_characteristics();
    let matrix = nclx.matrix_coefficients();
    if primaries == HeifPrimaries::Unknown
        || transfer == HeifTransfer::Unknown
        || matrix == HeifMatrix::Unknown
    {
        return Err(ViewerError::new(
            "unsupported_color_profile",
            "HEIF NCLX 含有未知色彩代碼，無法安全轉成 sRGB。",
        ));
    }
    let (primaries, transfer) = heif_nclx_fallback_codes(primaries as u8, transfer as u8);
    color_profile_from_cicp(primaries, transfer, true)
}

#[cfg(feature = "heic")]
fn decode_heif(bytes: Vec<u8>) -> Result<DecodedRender, ViewerError> {
    use libheif_rs::{
        ColorSpace, DecodingOptions, HeifContext, LibHeif, RgbChroma, SecurityLimits,
    };

    // Keep libheif's context and native decoded plane inside a narrow scope.
    // They are released before PNG encoding allocates its output buffer, which
    // avoids retaining two full RGBA planes plus the compressed result.
    let (width, height, rgba) = {
        let mut context = HeifContext::new()
            .map_err(|error| ViewerError::corrupt(format!("無法建立 HEIF 解碼器：{error}")))?;
        context.set_max_decoding_threads(1);
        let mut limits = SecurityLimits::new();
        limits.set_max_image_size_pixels(MAX_PIXELS);
        limits.set_max_memory_block_size(MAX_DECODE_BYTES);
        limits.set_max_total_memory(MAX_DECODE_BYTES);
        limits.set_max_color_profile_size(MAX_ICC_PROFILE_BYTES as u32);
        context.set_security_limits(&limits).map_err(|error| {
            ViewerError::limit(
                "decode_limit_exceeded",
                format!("無法套用 HEIF 安全限制：{error}"),
            )
        })?;
        // Set limits before parsing any attacker-controlled container structures.
        context
            .read_bytes(&bytes)
            .map_err(|error| ViewerError::corrupt(format!("HEIC/HEIF 容器無法讀取：{error}")))?;

        // The primary handle, rather than the first item ID, is intentional: a
        // multi-image HEIF file may designate any top-level image as primary.
        let handle = context
            .primary_image_handle()
            .map_err(|error| ViewerError::corrupt(format!("HEIF 找不到 primary image：{error}")))?;
        validate_dimensions(handle.width(), handle.height())?;
        let source_profile = heif_source_color_profile(&handle)?;
        let source_bit_depth = handle.luma_bits_per_pixel();
        let high_bit_depth = source_bit_depth > 8;
        if !matches!(source_bit_depth, 8 | 10 | 12 | 16) {
            return Err(ViewerError::new(
                "unsupported_bit_depth",
                format!("不支援 {source_bit_depth}-bit HEIC/HEIF 圖片。"),
            ));
        }
        if high_bit_depth {
            validate_high_bit_working_set(handle.width(), handle.height())?;
        }
        let mut options = DecodingOptions::new()
            .ok_or_else(|| ViewerError::corrupt("無法建立 HEIF 解碼選項。"))?;
        // Keep 10/12/16-bit samples intact until after colour conversion. libheif's
        // convert_hdr_to_8bit path only shifts bits and would discard precision.
        options.set_convert_hdr_to_8bit(false);
        options.set_strict_decoding(true);
        options.set_num_library_threads(1);
        options.set_num_codec_threads(1);
        let chroma = if high_bit_depth {
            RgbChroma::HdrRgbaLe
        } else {
            RgbChroma::Rgba
        };
        let image = LibHeif::new()
            .decode(&handle, ColorSpace::Rgb(chroma), Some(options))
            .map_err(|error| ViewerError::corrupt(format!("HEIC/HEIF 解碼失敗：{error}")))?;
        let premultiplied_alpha = image.is_premultiplied_alpha();
        let width = image.width();
        let height = image.height();
        validate_dimensions(width, height)?;
        let plane = image
            .planes()
            .interleaved
            .ok_or_else(|| ViewerError::corrupt("HEIC/HEIF 解碼結果缺少 RGBA 像素平面。"))?;
        if plane.width != width || plane.height != height {
            return Err(ViewerError::corrupt(
                "HEIC/HEIF 像素平面尺寸與 primary image 不一致。",
            ));
        }
        let bytes_per_pixel = if high_bit_depth { 8 } else { 4 };
        let row_bytes = (width as usize)
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "HEIF 列大小溢位。"))?;
        let plane_bytes = plane
            .stride
            .checked_mul(height as usize)
            .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "HEIF 像素平面大小溢位。"))?;
        if plane.stride < row_bytes || plane.data.len() < plane_bytes {
            return Err(ViewerError::corrupt("HEIC/HEIF 像素平面長度不正確。"));
        }
        let rgba = if high_bit_depth {
            if plane.bits_per_pixel != source_bit_depth || plane.storage_bits_per_pixel != 64 {
                return Err(ViewerError::corrupt(format!(
                    "HEIF 高位元像素格式不正確（{}-bit，儲存 {}-bit）。",
                    plane.bits_per_pixel, plane.storage_bits_per_pixel
                )));
            }
            let value_count = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "HEIF 像素數量溢位。"))?;
            let maximum = match source_bit_depth {
                10 => (1_u32 << 10) - 1,
                12 => (1_u32 << 12) - 1,
                16 => u32::from(u16::MAX),
                _ => unreachable!("high-bit HEIF depth was validated above"),
            };
            let mut rgba = try_vec_with_capacity(value_count)?;
            let row_value_count = (width as usize).checked_mul(4).ok_or_else(|| {
                ViewerError::limit("dimensions_exceeded", "HEIF 列像素數量溢位。")
            })?;
            let mut source_row = try_vec_with_capacity(row_value_count)?;
            source_row.resize(row_value_count, 0_u16);
            let transform = source_profile
                .as_ref()
                .map(|profile| create_transform_16(profile, source_bit_depth))
                .transpose()?;
            let mut transformed_row = if transform.is_some() {
                let mut row = try_vec_with_capacity(row_value_count)?;
                row.resize(row_value_count, 0_u16);
                row
            } else {
                Vec::new()
            };

            for row in 0..height as usize {
                let start = row * plane.stride;
                for (destination, value) in source_row
                    .iter_mut()
                    .zip(plane.data[start..start + row_bytes].chunks_exact(2))
                {
                    *destination = u16::from_le_bytes([value[0], value[1]]);
                }
                // Reject values outside the declared 10/12-bit domain before
                // alpha math or colour conversion can silently clamp them.
                if source_row.iter().any(|value| u32::from(*value) > maximum) {
                    return Err(ViewerError::corrupt(format!(
                        "HEIF 像素樣本超過宣告的 {source_bit_depth}-bit 範圍。"
                    )));
                }
                if premultiplied_alpha {
                    unpremultiply_rgba16(&mut source_row, source_bit_depth)?;
                }
                let quantize_source = if let Some(transform) = transform.as_ref() {
                    transform
                        .transform(&source_row, &mut transformed_row)
                        .map_err(|error| {
                            ViewerError::corrupt(format!("ICC 色彩轉換失敗：{error}"))
                        })?;
                    transformed_row.as_slice()
                } else {
                    source_row.as_slice()
                };
                rgba.extend(quantize_source.iter().map(|value| {
                    (((u32::from(*value).min(maximum) * 255) + maximum / 2) / maximum) as u8
                }));
            }
            rgba
        } else {
            if plane.bits_per_pixel != 8 || plane.storage_bits_per_pixel != 32 {
                return Err(ViewerError::corrupt(format!(
                    "HEIF RGBA 像素格式不正確（{}-bit，儲存 {}-bit）。",
                    plane.bits_per_pixel, plane.storage_bits_per_pixel
                )));
            }
            let capacity = row_bytes
                .checked_mul(height as usize)
                .ok_or_else(|| ViewerError::limit("dimensions_exceeded", "HEIF 像素大小溢位。"))?;
            let mut rgba = try_vec_with_capacity(capacity)?;
            for row in 0..height as usize {
                let start = row * plane.stride;
                rgba.extend_from_slice(&plane.data[start..start + row_bytes]);
            }
            if premultiplied_alpha {
                unpremultiply_rgba8(&mut rgba);
            }
            if let Some(profile) = source_profile.as_ref() {
                let transform = create_transform_8(profile)?;
                let mut image = image::RgbaImage::from_raw(width, height, rgba)
                    .ok_or_else(|| ViewerError::corrupt("HEIF RGBA 像素平面長度不正確。"))?;
                apply_rgb_color_transform(&mut image, &transform)?;
                image.into_raw()
            } else {
                rgba
            }
        };
        (width, height, rgba)
    };
    drop(bytes);
    let png = encode_rgba_png(width, height, &rgba)?;
    Ok(DecodedRender {
        bytes: png,
        mime_type: "image/png",
        width,
        height,
        animated: false,
    })
}

#[cfg(not(feature = "heic"))]
fn decode_heif(_bytes: Vec<u8>) -> Result<DecodedRender, ViewerError> {
    Err(ViewerError::new(
        "heic_unavailable",
        "這個開發版本未啟用 HEIC/HEIF 解碼；正式 Windows ZIP 會包含此功能。",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Frame, Rgba, RgbaImage};
    use std::borrow::Cow;
    use std::fs;
    use std::io::Cursor;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(name)
    }

    fn png_bytes(color: [u8; 4]) -> Vec<u8> {
        let mut output = Vec::new();
        image::codecs::png::PngEncoder::new(&mut output)
            .write_image(&color, 1, 1, ColorType::Rgba8.into())
            .unwrap();
        output
    }

    fn png_bytes_with_icc(color: [u8; 4], profile: &ColorProfile) -> Vec<u8> {
        let mut output = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut output);
        encoder.set_icc_profile(profile.encode().unwrap()).unwrap();
        encoder
            .write_image(&color, 1, 1, ColorType::Rgba8.into())
            .unwrap();
        output
    }

    fn png_bytes_with_chunks(
        mut info: png::Info<'static>,
        chunks: &[(png::chunk::ChunkType, Vec<u8>)],
        color: [u8; 4],
    ) -> Vec<u8> {
        info.width = 1;
        info.height = 1;
        info.color_type = png::ColorType::Rgba;
        info.bit_depth = png::BitDepth::Eight;
        let mut output = Vec::new();
        {
            let encoder = png::Encoder::with_info(&mut output, info).unwrap();
            let mut writer = encoder.write_header().unwrap();
            for (chunk, data) in chunks {
                writer.write_chunk(*chunk, data).unwrap();
            }
            writer.write_image_data(&color).unwrap();
        }
        output
    }

    #[test]
    fn magic_detection_does_not_trust_extensions() {
        assert_eq!(
            sniff_format(b"\xff\xd8\xffanything"),
            Some(SupportedFormat::Jpeg)
        );
        assert_eq!(sniff_format(b"GIF89aanything"), Some(SupportedFormat::Gif));
        assert_eq!(sniff_format(b"II*\0anything"), Some(SupportedFormat::Tiff));
        assert_eq!(sniff_format(b"not a supported file"), None);
    }

    #[test]
    fn disguised_extension_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fake.jpg");
        fs::write(&path, png_bytes([1, 2, 3, 255])).unwrap();
        let error = decode_file(&path).unwrap_err();
        assert_eq!(error.code, "format_mismatch");
    }

    #[test]
    fn dimension_limits_cover_side_and_total_pixels() {
        assert_eq!(
            validate_dimensions(MAX_SIDE + 1, 1).unwrap_err().code,
            "dimensions_exceeded"
        );
        assert_eq!(
            validate_dimensions(20_000, 20_000).unwrap_err().code,
            "dimensions_exceeded"
        );
        assert!(validate_dimensions(10_000, 10_000).is_ok());
    }

    #[test]
    fn high_bit_normalization_preflights_both_full_image_planes() {
        assert!(validate_high_bit_working_set(6_000, 6_000).is_ok());
        assert_eq!(
            validate_high_bit_working_set(8_000, 6_000)
                .unwrap_err()
                .code,
            "decode_limit_exceeded"
        );
    }

    #[test]
    fn metadata_limit_rejects_sparse_oversize_file_without_reading_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.png");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_INPUT_BYTES + 1).unwrap();
        drop(file);
        assert_eq!(decode_file(&path).unwrap_err().code, "file_too_large");
    }

    #[test]
    fn animated_gif_is_reported_and_original_bytes_are_kept() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("two.gif");
        let frames = [[255, 0, 0, 255], [0, 0, 255, 255]].map(|color| {
            Frame::from_parts(
                RgbaImage::from_pixel(1, 1, Rgba(color)),
                0,
                0,
                Delay::from_numer_denom_ms(10, 1),
            )
        });
        let mut bytes = Vec::new();
        image::codecs::gif::GifEncoder::new(&mut bytes)
            .encode_frames(frames)
            .unwrap();
        fs::write(&path, &bytes).unwrap();

        let decoded = decode_file(&path).unwrap();
        assert!(decoded.animated);
        assert_eq!(decoded.bytes, bytes);
        assert_eq!(decoded.mime_type, "image/gif");
    }

    #[test]
    fn tiff_conversion_uses_first_page_and_outputs_eight_bit_png() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pages.tiff");
        let mut bytes = Vec::new();
        {
            let mut encoder = tiff::encoder::TiffEncoder::new(Cursor::new(&mut bytes)).unwrap();
            encoder
                .new_image::<tiff::encoder::colortype::RGBA8>(1, 1)
                .unwrap()
                .write_data(&[255, 0, 0, 255])
                .unwrap();
            encoder
                .new_image::<tiff::encoder::colortype::RGBA8>(1, 1)
                .unwrap()
                .write_data(&[0, 0, 255, 255])
                .unwrap();
        }
        fs::write(&path, bytes).unwrap();

        let decoded = decode_file(&path).unwrap();
        assert_eq!(decoded.mime_type, "image/png");
        let image = image::load_from_memory(&decoded.bytes).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255]);
    }

    #[test]
    fn tiff_orientation_is_applied_before_png_conversion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oriented.tiff");
        let mut bytes = Vec::new();
        {
            let mut encoder = tiff::encoder::TiffEncoder::new(Cursor::new(&mut bytes)).unwrap();
            let mut image = encoder
                .new_image::<tiff::encoder::colortype::RGBA8>(2, 1)
                .unwrap();
            image
                .encoder()
                .write_tag(tiff::tags::Tag::Orientation, 6_u16)
                .unwrap();
            image.write_data(&[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        }
        fs::write(&path, bytes).unwrap();

        let decoded = decode_file(&path).unwrap();
        assert_eq!((decoded.width, decoded.height), (1, 2));
        let image = image::load_from_memory(&decoded.bytes).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(0, 1).0, [0, 0, 255, 255]);
    }

    #[test]
    fn decoder_drops_file_handle_after_reading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pixel.png");
        let renamed = directory.path().join("renamed.png");
        fs::write(&path, png_bytes([9, 8, 7, 255])).unwrap();
        decode_file(&path).unwrap();
        fs::rename(&path, &renamed).unwrap();
        fs::remove_file(renamed).unwrap();
    }

    #[test]
    fn committed_animation_and_invalid_fixtures_match_contract() {
        let webp = decode_file(&fixture("animated.webp")).unwrap();
        assert!(webp.animated);
        assert_eq!(webp.mime_type, "image/webp");
        assert_eq!(
            decode_file(&fixture("corrupt.jpg")).unwrap_err().code,
            "corrupt_image"
        );
        assert_eq!(
            decode_file(&fixture("disguised.jpg")).unwrap_err().code,
            "format_mismatch"
        );
        assert_eq!(
            decode_file(&fixture("oversize-width.png"))
                .unwrap_err()
                .code,
            "dimensions_exceeded"
        );
    }

    #[test]
    fn committed_sixteen_bit_png_is_normalized_to_eight_bit_rgba() {
        let source = fs::read(fixture("sixteen-bit.png")).unwrap();
        assert_eq!(source[24], 16, "fixture must remain a 16-bit PNG");

        let decoded = decode_file(&fixture("sixteen-bit.png")).unwrap();
        assert_eq!(decoded.mime_type, "image/png");
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!(decoded.bytes[24], 8, "render PNG must be 8-bit");
        assert_ne!(decoded.bytes, source);
        assert_eq!(
            image::load_from_memory(&decoded.bytes).unwrap().color(),
            ColorType::Rgba8
        );
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(&decoded.bytes)).unwrap();
        assert!(decoder.icc_profile().unwrap().is_some());
    }

    #[test]
    fn sixteen_bit_color_conversion_happens_before_quantization() {
        let source = vec![1_000_u16, 2_000, 9_912, u16::MAX];
        let mut image =
            image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_raw(1, 1, source).unwrap();
        apply_profile_transform_16(&mut image, &ColorProfile::new_display_p3(), 16).unwrap();
        let actual = quantize_rgba16(image.as_raw(), 16).unwrap();

        // Independent Display-P3 -> sRGB calculation at full u16 precision.
        assert_eq!(actual, [3, 8, 40, 255]);
        assert_ne!(
            actual,
            [3, 8, 41, 255],
            "quantizing the source first would round blue to the wrong value"
        );
    }

    #[test]
    fn wide_gamut_static_png_is_converted_and_tagged_srgb() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("display-p3.png");
        let source_profile = ColorProfile::new_display_p3();
        let source_pixel = [32, 160, 224, 255];
        let source = png_bytes_with_icc(source_pixel, &source_profile);
        fs::write(&path, &source).unwrap();

        // Independent Display-P3 -> sRGB matrix/TRC reference, rounded to u8.
        let expected = [0, 163, 230, 255];
        assert_ne!(expected, source_pixel, "fixture must exercise conversion");

        let decoded = decode_file(&path).unwrap();
        assert_eq!(decoded.mime_type, "image/png");
        assert_ne!(decoded.bytes, source);
        let actual = image::load_from_memory(&decoded.bytes)
            .unwrap()
            .to_rgba8()
            .get_pixel(0, 0)
            .0;
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.abs_diff(expected) <= 1),
            "expected independent P3 reference {expected:?}, got {actual:?}"
        );

        let mut png_decoder =
            image::codecs::png::PngDecoder::new(Cursor::new(&decoded.bytes)).unwrap();
        let output_icc = png_decoder.icc_profile().unwrap().unwrap();
        let output_profile = ColorProfile::new_from_slice(&output_icc).unwrap();
        let output_transform = output_profile
            .create_transform_8bit(
                Layout::Rgba,
                &ColorProfile::new_srgb(),
                Layout::Rgba,
                Default::default(),
            )
            .unwrap();
        let mut round_trip = [0_u8; 4];
        output_transform
            .transform(&actual, &mut round_trip)
            .unwrap();
        assert!(
            actual
                .iter()
                .zip(round_trip)
                .all(|(left, right)| left.abs_diff(right) <= 1),
            "output ICC must describe sRGB pixels"
        );
    }

    #[test]
    fn equivalent_srgb_icc_keeps_original_static_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("srgb.png");
        let source = png_bytes_with_icc([32, 160, 224, 255], &ColorProfile::new_srgb());
        fs::write(&path, &source).unwrap();

        let decoded = decode_file(&path).unwrap();
        assert_eq!(decoded.bytes, source);
    }

    #[test]
    fn non_rgb_icc_is_never_retagged_after_mandatory_normalization() {
        let profile = ColorProfile::new_gray_with_gamma(2.2).encode().unwrap();
        assert!(parse_rgb_icc_profile(&profile, false).unwrap().is_none());
        assert_eq!(
            parse_rgb_icc_profile(&profile, true).unwrap_err().code,
            "unsupported_color_profile"
        );
    }

    #[test]
    fn cicp_profiles_distinguish_srgb_and_wide_gamut() {
        assert!(
            color_profile_from_cicp(1, 13, true).unwrap().is_none(),
            "BT.709 primaries with the sRGB transfer is structurally sRGB"
        );
        assert!(
            color_profile_from_cicp(9, 13, true).unwrap().is_some(),
            "BT.2020 primaries must be converted"
        );
        assert_eq!(
            color_profile_from_cicp(2, 2, true).unwrap_err().code,
            "unsupported_color_profile"
        );
    }

    #[test]
    fn heif_nclx_unspecified_fields_fall_back_independently() {
        assert_eq!(heif_nclx_fallback_codes(2, 2), (1, 13));
        assert_eq!(heif_nclx_fallback_codes(2, 16), (1, 16));
        assert_eq!(heif_nclx_fallback_codes(9, 2), (9, 13));
        assert_eq!(heif_nclx_fallback_codes(9, 16), (9, 16));
    }

    #[test]
    fn png_cicp_has_precedence_over_conflicting_icc() {
        let mut info = png::Info::default();
        info.icc_profile = Some(Cow::Owned(ColorProfile::new_srgb().encode().unwrap()));
        let bytes = png_bytes_with_chunks(
            info,
            &[(png::chunk::cICP, vec![9, 13, 0, 1])],
            [32, 160, 224, 255],
        );

        let reader = png::Decoder::new(Cursor::new(&bytes)).read_info().unwrap();
        assert!(reader.info().coding_independent_code_points.is_some());
        assert!(reader.info().icc_profile.is_some());
        assert!(matches!(
            png_color_source(&bytes).unwrap(),
            PngColorSource::ExplicitProfile(_)
        ));
    }

    #[test]
    fn png_chrm_and_gama_are_read_from_decoder_fields() {
        let chromaticities = png::SourceChromaticities::new(
            (0.3127, 0.3290),
            (0.6800, 0.3200),
            (0.2650, 0.6900),
            (0.1500, 0.0600),
        );
        let gamma = png::ScaledFloat::new(1.0 / 2.2);
        let bytes = png_bytes_with_chunks(
            png::Info::default(),
            &[
                (png::chunk::cHRM, chromaticities.to_be_bytes().to_vec()),
                (png::chunk::gAMA, gamma.into_scaled().to_be_bytes().to_vec()),
            ],
            [32, 160, 224, 255],
        );

        assert!(matches!(
            png_color_source(&bytes).unwrap(),
            PngColorSource::ExplicitProfile(_)
        ));
    }

    #[test]
    fn invalid_png_chrm_coordinates_are_rejected() {
        let invalid_coordinates = png::SourceChromaticities::new(
            (0.3127, 0.3290),
            (0.8000, 0.3000),
            (0.2650, 0.6900),
            (0.1500, 0.0600),
        );
        let bytes = png_bytes_with_chunks(
            png::Info::default(),
            &[
                (png::chunk::cHRM, invalid_coordinates.to_be_bytes().to_vec()),
                (
                    png::chunk::gAMA,
                    png::ScaledFloat::new(1.0 / 2.2)
                        .into_scaled()
                        .to_be_bytes()
                        .to_vec(),
                ),
            ],
            [32, 160, 224, 255],
        );

        assert_eq!(
            png_color_source(&bytes).unwrap_err().code,
            "unsupported_color_profile"
        );
        assert!(!valid_png_chromaticity(Chromaticity::new(f32::NAN, 0.3)));
    }

    #[test]
    fn degenerate_png_chrm_colorants_are_rejected() {
        let degenerate = png::SourceChromaticities::new(
            (0.3127, 0.3290),
            (0.6400, 0.3300),
            (0.6400, 0.3300),
            (0.6400, 0.3300),
        );
        let bytes = png_bytes_with_chunks(
            png::Info::default(),
            &[
                (png::chunk::cHRM, degenerate.to_be_bytes().to_vec()),
                (
                    png::chunk::gAMA,
                    png::ScaledFloat::new(1.0 / 2.2)
                        .into_scaled()
                        .to_be_bytes()
                        .to_vec(),
                ),
            ],
            [32, 160, 224, 255],
        );

        assert_eq!(
            png_color_source(&bytes).unwrap_err().code,
            "unsupported_color_profile"
        );
    }

    #[test]
    fn narrow_range_png_cicp_is_a_recoverable_error() {
        let bytes = png_bytes_with_chunks(
            png::Info::default(),
            &[(png::chunk::cICP, vec![1, 13, 0, 0])],
            [16, 128, 235, 255],
        );
        assert_eq!(
            png_color_source(&bytes).unwrap_err().code,
            "unsupported_color_profile"
        );
    }

    #[test]
    fn premultiplied_alpha_is_restored_before_color_conversion() {
        let mut rgba8 = [64, 32, 8, 128, 9, 8, 7, 0];
        unpremultiply_rgba8(&mut rgba8);
        assert_eq!(rgba8, [128, 64, 16, 128, 0, 0, 0, 0]);

        let mut rgba10 = [256_u16, 128, 32, 512, 9, 8, 7, 0];
        unpremultiply_rgba16(&mut rgba10, 10).unwrap();
        assert_eq!(rgba10, [512, 256, 64, 512, 0, 0, 0, 0]);
    }

    #[test]
    fn committed_two_page_tiff_uses_expected_first_page_dimensions() {
        let decoded = decode_file(&fixture("two-page.tiff")).unwrap();
        assert_eq!((decoded.width, decoded.height), (5, 3));
    }

    #[test]
    fn raw_jpeg_descriptor_uses_exif_oriented_dimensions() {
        let decoded = decode_file(&fixture("exif-rotated.jpg")).unwrap();
        assert_eq!(decoded.mime_type, "image/jpeg");
        assert_eq!((decoded.width, decoded.height), (3, 6));
        assert!(decoded.bytes.starts_with(&[0xff, 0xd8, 0xff]));
    }

    #[cfg(feature = "heic")]
    #[test]
    fn libheif_runtime_plugin_loading_is_disabled() {
        // The portable build deliberately uses only libheif's built-in
        // libde265 decoder. A non-empty list here would reintroduce runtime
        // DLL scanning, including the empty-path drive-root fallback on Windows.
        unsafe {
            let directories = libheif_sys::heif_get_plugin_directories();
            assert!(!directories.is_null());
            let is_empty = (*directories).is_null();
            libheif_sys::heif_free_plugin_directories(directories);
            assert!(
                is_empty,
                "libheif runtime plugin loading must stay disabled"
            );
        }
    }

    #[cfg(feature = "heic")]
    #[test]
    fn committed_heif_fixtures_decode_primary_and_heif_extension() {
        let primary = decode_file(&fixture("primary-second.heic")).unwrap();
        assert_eq!(primary.mime_type, "image/png");
        assert_eq!((primary.width, primary.height), (3, 5));
        assert!(primary.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let primary_pixel = image::load_from_memory(&primary.bytes)
            .unwrap()
            .to_rgb8()
            .get_pixel(0, 0)
            .0;
        assert!(
            primary_pixel[2] > 150
                && primary_pixel[2] > primary_pixel[0]
                && primary_pixel[2] > primary_pixel[1],
            "expected the blue designated primary item, got {primary_pixel:?}"
        );

        let single = decode_file(&fixture("single.heif")).unwrap();
        assert_eq!((single.width, single.height), (4, 3));
        assert!(single.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let single_pixel = image::load_from_memory(&single.bytes)
            .unwrap()
            .to_rgb8()
            .get_pixel(0, 0)
            .0;
        assert!(
            single_pixel[1] > 100
                && single_pixel[1] > single_pixel[0]
                && single_pixel[1] > single_pixel[2],
            "expected green .heif pixels, got {single_pixel:?}"
        );
    }
}
