# Third-party notices

ImgViewer is distributed under the MIT License. It incorporates or dynamically
links third-party components. This notice is informational; each component's
license text controls.

## Portable Windows runtime

The portable ZIP dynamically links these LGPL components:

| Component | Version | License | Source |
| --- | --- | --- | --- |
| libheif | 1.21.2 | LGPL-3.0-only | <https://github.com/strukturag/libheif/tree/v1.21.2> |
| libde265 | 1.0.18 | LGPL-3.0-only | <https://github.com/strukturag/libde265/tree/v1.0.18> |

They are built by the vcpkg `2026.05.25` ports at commit
`d015e31e90838a4c9dfa3eed45979bc70d9357fc`. The manifest disables libheif's
default features. Consequently the package keeps the libde265 HEVC decoder and
does not include the GPL x265 encoder or optional AVIF codecs. The checked-in
libheif overlay is the same pinned port with runtime plugin loading disabled;
the libde265 decoder remains linked into libheif's built-in decoder registry.

The release ZIP places the exact license texts in `licenses/libheif.txt` and
`licenses/libde265.txt`, and records source versions and URLs in
`SOURCE_VERSIONS.txt`. The DLLs sit next to `ImgViewer.exe` and may be replaced
with ABI-compatible modified builds. No code-signature or integrity mechanism
prevents that replacement.

The package may also contain Microsoft Visual C++ runtime DLLs copied from the
installed Visual Studio redistributable directory. Microsoft licenses those
files under the Visual Studio license terms. They are not open-source
components.

The Microsoft Edge WebView2 Evergreen Runtime is **not** included in the ZIP.
ImgViewer uses the compatible runtime already installed on Windows.

## Application libraries

The executable also incorporates open-source libraries, including:

- Tauri and its official plugins — Apache-2.0 OR MIT.
- Vue — MIT.
- `image` — MIT OR Apache-2.0.
- `moxcms` — BSD-3-Clause OR Apache-2.0; used for pure-Rust ICC/CICP/chromaticity-to-sRGB conversion.
- `png` — MIT OR Apache-2.0; used to read bounded PNG cICP, cHRM, gAMA and ICC metadata.
- `libheif-rs` and `libheif-sys` — MIT.
- Serde, Rand, and Parking Lot — permissive MIT/Apache-family licenses as
  declared by their packages.

The committed `Cargo.lock` and `pnpm-lock.yaml` are the authoritative version
inventory for transitive Rust and frontend dependencies. Build-only tools such
as Vite and Vitest are not shipped as separate runtime programs.
