fn main() {
    let app_manifest = tauri_build::AppManifest::new().commands(&[
        "open_path",
        "navigate",
        "current_snapshot",
        "read_render",
    ]);
    let attributes = tauri_build::Attributes::new().app_manifest(app_manifest);

    #[cfg(windows)]
    {
        // generate_context! also resolves the configured icon during crate
        // compilation, so place this deterministic build artifact at the
        // static path declared in tauri.conf.json.
        let icon = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"),
        )
        .join("target/generated/imgviewer.ico");
        std::fs::create_dir_all(icon.parent().expect("generated icon parent"))
            .expect("create generated icon directory");
        std::fs::write(&icon, minimal_windows_icon()).expect("write generated Windows icon");
        let attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new().window_icon_path(icon));
        tauri_build::try_build(attributes).expect("run Tauri build script");
    }
    #[cfg(not(windows))]
    tauri_build::try_build(attributes).expect("run Tauri build script");
}

#[cfg(windows)]
fn minimal_windows_icon() -> Vec<u8> {
    // A deterministic 1x1 BGRA icon is sufficient for the PE resource. The
    // packaged application can later provide a branded multi-resolution icon
    // without making local checks depend on a binary repository asset.
    let mut icon = vec![
        0, 0, // reserved
        1, 0, // ICO type
        1, 0, // one image
        1, 1, 0, 0, // width, height, palette, reserved
        1, 0, 32, 0, // planes, bits per pixel
        48, 0, 0, 0, // bytes in image
        22, 0, 0, 0, // image offset
        40, 0, 0, 0, // BITMAPINFOHEADER size
        1, 0, 0, 0, // width
        2, 0, 0, 0, // doubled height (XOR + AND)
        1, 0, 32, 0, // planes, bits per pixel
        0, 0, 0, 0, // compression
        4, 0, 0, 0, // image byte size
        0, 0, 0, 0, 0, 0, 0, 0, // pixel density
        0, 0, 0, 0, 0, 0, 0, 0, // palette metadata
    ];
    icon.extend_from_slice(&[0x35, 0x87, 0xe8, 0xff]); // one opaque BGRA pixel
    icon.extend_from_slice(&[0, 0, 0, 0]); // padded AND mask
    icon
}
