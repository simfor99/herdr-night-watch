#!/usr/bin/env python3
"""Build the Windows EXE icon from the Material bedtime glyph."""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parents[1]
SOURCE_PNG = ROOT / "assets" / "material-bedtime-32.png"
OUTPUT_ICO = ROOT / "assets" / "herdr-nachtwaechter.ico"
OUTPUT_PNG = ROOT / "assets" / "herdr-nachtwaechter-256.png"

# Same idle/off tray tint as src/tray.rs
MOON = (96, 165, 250, 255)
PLATE = (14, 19, 33, 255)
PLATE_EDGE = (40, 52, 78, 255)
SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)


def moon_mask(size: int) -> Image.Image:
    source = Image.open(SOURCE_PNG).convert("RGBA")
    alpha = source.split()[3]
    return alpha.resize((size, size), Image.Resampling.LANCZOS)


def render(size: int) -> Image.Image:
    image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    inset = max(1, round(size * 0.06))
    radius = max(3, round(size * 0.28))
    box = (inset, inset, size - inset - 1, size - inset - 1)
    draw.rounded_rectangle(box, radius=radius, fill=PLATE, outline=PLATE_EDGE, width=max(1, size // 64))

    glyph = max(12, round(size * 0.62))
    mask = moon_mask(glyph)
    moon = Image.new("RGBA", (glyph, glyph), MOON)
    moon.putalpha(mask)
    origin = ((size - glyph) // 2, (size - glyph) // 2 - max(0, size // 48))
    image.alpha_composite(moon, origin)
    if size >= 48:
        image = image.filter(ImageFilter.UnsharpMask(radius=0.6, percent=80, threshold=2))
    return image


def main() -> None:
    masters = {size: render(size) for size in SIZES}
    masters[256].save(OUTPUT_PNG)
    # Pillow keeps only one ICO frame unless the largest image is the source
    # and `sizes` lists every Windows explorer slot.
    masters[256].save(
        OUTPUT_ICO,
        format="ICO",
        sizes=[(size, size) for size in SIZES],
    )
    print(f"wrote {OUTPUT_ICO.relative_to(ROOT)} and {OUTPUT_PNG.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
