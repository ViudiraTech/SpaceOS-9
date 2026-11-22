#!/usr/bin/env python3
"""Render specimen previews for the font packages.

Mature font catalogues (Google Fonts, Fontbase) sell a face with a specimen
card: the family name set in the face itself plus a sample line. This does the
same in the store's 960x540 preview size, using the actual TTF that ships in
the package, so the picture always matches the bytes users install.

Output: build/previews/<package>.jpg and <package>-2.jpg
"""

from __future__ import annotations

import pathlib
import sys

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "build" / "previews"

# package -> (font file inside the staged tree, family, accent, second accent)
FONTS = {
    "font-hack": (
        "usr/share/fonts/hack/Hack-Regular.ttf",
        "Hack",
        (0x22, 0xd3, 0xee),
        (0x08, 0x91, 0xb2),
    ),
    "font-carlito": (
        "usr/share/fonts/carlito/Carlito-Regular.ttf",
        "Carlito",
        (0xf5, 0x9e, 0x0b),
        (0xb4, 0x53, 0x09),
    ),
    "font-croscore": (
        "usr/share/fonts/croscore/Tinos-Regular.ttf",
        "Tinos",
        (0xa7, 0x8b, 0xfa),
        (0x6d, 0x28, 0xd9),
    ),
}

LICENSE = {
    "font-hack": "MIT",
    "font-carlito": "SIL OFL 1.1",
    "font-croscore": "Apache-2.0",
}

# The packaged faces are Latin-only, so the specimen stays Latin as well.
SAMPLE = "Aa Bb Cc Dd 0123456789"
SAMPLE_2 = "The quick brown fox jumps over the lazy dog"


def gradient(top: tuple[int, int, int], bottom: tuple[int, int, int]) -> Image.Image:
    width, height = 960, 540
    image = Image.new("RGB", (width, height))
    draw = ImageDraw.Draw(image)
    for y in range(height):
        t = y / (height - 1)
        color = tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3))
        draw.line([(0, y), (width, y)], fill=color)
    return image


def add_glow(image: Image.Image, center: tuple[int, int], radius: int, color: tuple[int, int, int]) -> None:
    layer = Image.new("RGBA", image.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(layer)
    draw.ellipse(
        [center[0] - radius, center[1] - radius, center[0] + radius, center[1] + radius],
        fill=color + (110,),
    )
    layer = layer.filter(ImageFilter.GaussianBlur(radius // 2))
    image.paste(layer, (0, 0), layer)


def card(
    image: Image.Image,
    box: tuple[int, int, int, int],
    font_path: pathlib.Path,
    family: str,
    accent: tuple[int, int, int],
    sample: str,
    subtitle: str,
) -> None:
    layer = Image.new("RGBA", image.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(layer)
    # soft contact shadow
    shadow = Image.new("RGBA", image.size, (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle(
        [box[0] + 6, box[1] + 14, box[2] + 6, box[3] + 14], radius=26, fill=(0, 0, 0, 120)
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(18))
    image.paste(shadow, (0, 0), shadow)

    draw.rounded_rectangle(box, radius=26, fill=(15, 18, 28, 226), outline=accent + (150,), width=2)
    image.paste(layer, (0, 0), layer)

    title_font = ImageFont.truetype(str(font_path), 72)
    sample_font = ImageFont.truetype(str(font_path), 30)
    caption_font = ImageFont.truetype(str(font_path), 17)

    # Draw on the composed image: text must land after the glass card.
    ink = ImageDraw.Draw(image)
    x = box[0] + 54
    ink.text((x, box[1] + 54), family, font=title_font, fill=(255, 255, 255))
    ink.text((x, box[1] + 168), sample, font=sample_font, fill=accent)
    ink.text((x, box[3] - 62), subtitle, font=caption_font, fill=(226, 232, 240))


def render(package: str, font_file: str, family: str, accent, accent2, variant: int) -> pathlib.Path:
    stage = ROOT / "build" / "packages" / package
    font_path = stage / font_file
    if not font_path.is_file():
        raise SystemExit(f"{font_path} is missing; run make packages first")

    top = (12, 16, 28)
    bottom = tuple(max(0, c // 3) for c in accent2)
    image = gradient(top, bottom)
    add_glow(image, (760, 120), 300, accent)
    add_glow(image, (160, 470), 260, accent2)

    sample = SAMPLE if variant == 1 else SAMPLE_2
    subtitle = f"SpaceOS Font Pack  ·  {LICENSE[package]}  ·  {family}"
    card(image, (80, 70, 880, 470), font_path, family, accent, sample, subtitle)

    OUT.mkdir(parents=True, exist_ok=True)
    suffix = "" if variant == 1 else "-2"
    path = OUT / f"{package}{suffix}.jpg"
    image.save(path, "JPEG", quality=92, optimize=True)
    return path


def main() -> int:
    for package, (font_file, family, accent, accent2) in FONTS.items():
        for variant in (1, 2):
            print(render(package, font_file, family, accent, accent2, variant))
    return 0


if __name__ == "__main__":
    sys.exit(main())
