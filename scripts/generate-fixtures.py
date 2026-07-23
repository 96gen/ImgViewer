"""Generate the small, deterministic ImgViewer format fixtures.

Requirements:
    python -m pip install Pillow==12.3.0 pillow-heif==1.4.0

The generated binary files are committed so normal test runs do not need
Python, Pillow, an HEIF system extension, or network access.
"""

from __future__ import annotations

import shutil
import struct
import zlib
from pathlib import Path

try:
    from PIL import Image
    import pillow_heif
except ImportError as exc:  # pragma: no cover - maintainer utility only
    raise SystemExit(
        "Install generator dependencies with: "
        "python -m pip install Pillow==12.3.0 pillow-heif==1.4.0"
    ) from exc


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "tests" / "fixtures"


def solid(mode: str, size: tuple[int, int], value: tuple[int, ...] | int) -> Image.Image:
    return Image.new(mode, size, value)


def png_chunk(kind: bytes, payload: bytes) -> bytes:
    checksum = zlib.crc32(kind + payload) & 0xFFFFFFFF
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", checksum)


def make_oversize_png() -> bytes:
    signature = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", 32_769, 1, 8, 2, 0, 0, 0)
    return signature + png_chunk(b"IHDR", ihdr) + png_chunk(b"IEND", b"")


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    pillow_heif.register_heif_opener()

    # Natural-sort fixtures encode their intended order in both name and color.
    for name, color in (
        ("1.jpg", (220, 30, 30)),
        ("2.jpg", (30, 200, 30)),
        ("10.jpg", (30, 30, 220)),
    ):
        solid("RGB", (4, 3), color).save(OUTPUT / name, "JPEG", quality=95, subsampling=0)

    exif = Image.Exif()
    exif[274] = 6  # Rotate 90 degrees clockwise for display.
    rotated = solid("RGB", (6, 3), (235, 180, 20))
    rotated.putpixel((0, 0), (0, 0, 0))
    rotated.save(OUTPUT / "exif-rotated.jpg", "JPEG", quality=95, subsampling=0, exif=exif)

    transparent = solid("RGBA", (4, 3), (0, 120, 255, 0))
    transparent.putpixel((1, 1), (255, 0, 100, 128))
    transparent.putpixel((2, 1), (255, 255, 255, 255))
    transparent.save(OUTPUT / "transparent.png", "PNG")
    solid("RGBA", (1, 1), (50, 100, 150, 255)).save(OUTPUT / "one-pixel.png", "PNG")

    sixteen_bit = Image.new("I;16", (2, 2))
    sixteen_bit.putdata([0, 16_384, 32_768, 65_535])
    sixteen_bit.save(OUTPUT / "sixteen-bit.png", "PNG")

    frames = [
        solid("RGBA", (3, 2), (230, 20, 20, 255)),
        solid("RGBA", (3, 2), (20, 210, 20, 180)),
        solid("RGBA", (3, 2), (20, 20, 230, 255)),
    ]
    frames[0].save(
        OUTPUT / "animated.gif",
        "GIF",
        save_all=True,
        append_images=frames[1:],
        duration=[80, 120, 160],
        loop=0,
        disposal=2,
    )
    frames[0].save(
        OUTPUT / "animated.webp",
        "WEBP",
        save_all=True,
        append_images=frames[1:],
        duration=[80, 120, 160],
        loop=0,
        lossless=True,
        quality=100,
        method=6,
    )

    tiff_first = solid("RGBA", (5, 3), (180, 20, 180, 255))
    tiff_second = solid("RGBA", (2, 6), (20, 180, 180, 128))
    tiff_first.save(
        OUTPUT / "two-page.tiff",
        "TIFF",
        save_all=True,
        append_images=[tiff_second],
        compression="raw",
    )

    heif_non_primary = solid("RGB", (4, 2), (225, 40, 40))
    heif_primary = solid("RGB", (3, 5), (35, 70, 225))
    heif_non_primary.save(
        OUTPUT / "primary-second.heic",
        "HEIF",
        save_all=True,
        append_images=[heif_primary],
        primary_index=1,
        quality=95,
    )
    solid("RGB", (4, 3), (70, 190, 100)).save(
        OUTPUT / "single.heif", "HEIF", quality=95
    )

    (OUTPUT / "oversize-width.png").write_bytes(make_oversize_png())
    (OUTPUT / "corrupt.jpg").write_bytes(b"\xff\xd8\xff\xe0\x00\x10JFIF\x00truncated")
    shutil.copyfile(OUTPUT / "transparent.png", OUTPUT / "disguised.jpg")


if __name__ == "__main__":
    main()
