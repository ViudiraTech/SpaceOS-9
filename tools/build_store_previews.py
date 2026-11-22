#!/usr/bin/env python3
"""Generate 720x360 JPEG preview banners for Space 商店 theme packages."""

import pathlib
from PIL import Image, ImageDraw, ImageFilter

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "packages" / "previews"
SIZE = (720, 360)


def cover_crop(path: pathlib.Path, size: tuple[int, int]) -> Image.Image:
    im = Image.open(path).convert("RGB")
    tw, th = size
    sw, sh = im.size
    scale = max(tw / sw, th / sh)
    nw, nh = max(tw, round(sw * scale)), max(th, round(sh * scale))
    im = im.resize((nw, nh), Image.Resampling.LANCZOS)
    x0 = (nw - tw) // 2
    y0 = (nh - th) // 2
    return im.crop((x0, y0, x0 + tw, y0 + th)).convert("RGBA")


def vgrad(w: int, h: int, top: tuple[int, int, int, int], bot: tuple[int, int, int, int]) -> Image.Image:
    grad = Image.new("RGBA", (w, h))
    px = []
    for y in range(h):
        t = y / max(1, h - 1)
        row_c = (
            round(top[0] + (bot[0] - top[0]) * t),
            round(top[1] + (bot[1] - top[1]) * t),
            round(top[2] + (bot[2] - top[2]) * t),
            round(top[3] + (bot[3] - top[3]) * t),
        )
        px.extend([row_c] * w)
    grad.putdata(px)
    return grad


def round_mask(size: tuple[int, int], radius: int, corners=(True, True, True, True)) -> Image.Image:
    w, h = size
    mask = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(mask)
    d.rounded_rectangle([0, 0, w - 1, h - 1], radius=radius, fill=255)
    tl, tr, br, bl = corners
    if not tl:
        d.rectangle([0, 0, radius, radius], fill=255)
    if not tr:
        d.rectangle([w - radius - 1, 0, w - 1, radius], fill=255)
    if not br:
        d.rectangle([w - radius - 1, h - radius - 1, w - 1, h - 1], fill=255)
    if not bl:
        d.rectangle([0, h - radius - 1, radius, h - 1], fill=255)
    return mask


def add_shadow(canvas: Image.Image, box: tuple[int, int, int, int], radius: int, alpha: int = 96, blur: int = 12) -> None:
    x0, y0, x1, y1 = box
    sh = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(sh)
    d.rounded_rectangle([x0, y0 + 4, x1, y1 + 4], radius=radius, fill=(10, 14, 24, alpha))
    sh = sh.filter(ImageFilter.GaussianBlur(blur))
    canvas.alpha_composite(sh)


def draw_mock_content(draw: ImageDraw.ImageDraw, body: tuple[int, int, int, int], sidebar_bg, card_bg, border_c, accent_c, text_c, muted_c) -> None:
    x0, y0, x1, y1 = body
    draw.rectangle([x0, y0, x0 + 96, y1], fill=sidebar_bg)
    draw.line([(x0 + 96, y0), (x0 + 96, y1)], fill=border_c, width=1)
    # Sidebar pills
    draw.rounded_rectangle([x0 + 12, y0 + 16, x0 + 84, y0 + 34], radius=6, fill=accent_c)
    for idx in range(3):
        yy = y0 + 44 + idx * 24
        draw.rounded_rectangle([x0 + 12, yy, x0 + 76 - idx * 6, yy + 12], radius=4, fill=muted_c)
    # Main mini cards
    cx0 = x0 + 112
    draw.rounded_rectangle([cx0, y0 + 16, x1 - 16, y0 + 82], radius=8, fill=accent_c)
    draw.rounded_rectangle([cx0 + 14, y0 + 30, cx0 + 140, y0 + 44], radius=5, fill=(255, 255, 255, 225))
    draw.rounded_rectangle([cx0 + 14, y0 + 52, cx0 + 210, y0 + 62], radius=4, fill=(255, 255, 255, 165))
    card_w = (x1 - 16 - cx0 - 12) // 2
    for i in range(2):
        kx = cx0 + i * (card_w + 12)
        ky = y0 + 94
        draw.rounded_rectangle([kx, ky, kx + card_w, y1 - 16], radius=8, fill=card_bg, outline=border_c, width=1)
        draw.rounded_rectangle([kx + 12, ky + 14, kx + 42, ky + 44], radius=8, fill=accent_c)
        draw.rounded_rectangle([kx + 52, ky + 18, kx + card_w - 16, ky + 28], radius=4, fill=text_c)
        draw.rounded_rectangle([kx + 52, ky + 34, kx + card_w - 38, ky + 42], radius=4, fill=muted_c)


def build_winxp() -> Image.Image:
    base = cover_crop(ROOT / "packages/theme-winxp/winxp-bliss.jpg", SIZE)
    # Back soft window
    add_shadow(base, (72, 28, 340, 210), 8, alpha=70, blur=10)
    d = ImageDraw.Draw(base)
    d.rounded_rectangle([72, 28, 340, 210], radius=8, fill=(236, 233, 216, 255), outline=(0, 84, 227, 255), width=2)
    d.rounded_rectangle([72, 28, 340, 52], radius=6, fill=(49, 106, 197, 255))

    # Main hero window
    wx0, wy0, wx1, wy1 = 152, 52, 624, 294
    add_shadow(base, (wx0, wy0, wx1, wy1), 9, alpha=110, blur=12)
    d = ImageDraw.Draw(base)
    d.rounded_rectangle([wx0, wy0, wx1, wy1], radius=9, fill=(241, 239, 226, 255), outline=(0, 72, 196, 255), width=2)
    tbar = vgrad(wx1 - wx0 - 4, 28, (35, 131, 255, 255), (0, 78, 215, 255))
    tbar.putalpha(round_mask((wx1 - wx0 - 4, 28), 7, (True, True, False, False)))
    base.alpha_composite(tbar, (wx0 + 2, wy0 + 2))
    d = ImageDraw.Draw(base)
    d.rounded_rectangle([wx0 + 14, wy0 + 10, wx0 + 128, wy0 + 20], radius=4, fill=(255, 255, 255, 220))
    for i, fill_c in enumerate([(44, 118, 240, 255), (44, 118, 240, 255), (229, 76, 52, 255)]):
        bx0 = wx1 - 68 + i * 20
        d.rounded_rectangle([bx0, wy0 + 6, bx0 + 17, wy0 + 23], radius=3, fill=fill_c, outline=(255, 255, 255, 210), width=1)
    draw_mock_content(
        d,
        (wx0 + 4, wy0 + 32, wx1 - 4, wy1 - 4),
        (225, 233, 248, 255),
        (255, 255, 255, 255),
        (195, 206, 225, 255),
        (37, 99, 235, 255),
        (30, 41, 59, 200),
        (148, 163, 184, 180),
    )
    # Luna bottom taskbar
    bar = vgrad(SIZE[0], 34, (45, 128, 248, 255), (21, 78, 194, 255))
    base.alpha_composite(bar, (0, SIZE[1] - 34))
    d = ImageDraw.Draw(base)
    start_btn = vgrad(92, 34, (108, 198, 92, 255), (42, 134, 42, 255))
    start_btn.putalpha(round_mask((92, 34), 12, (False, True, True, False)))
    base.alpha_composite(start_btn, (0, SIZE[1] - 34))
    d = ImageDraw.Draw(base)
    d.rounded_rectangle([18, SIZE[1] - 22, 72, SIZE[1] - 12], radius=4, fill=(255, 255, 255, 225))
    d.rounded_rectangle([108, SIZE[1] - 28, 220, SIZE[1] - 6], radius=4, fill=(65, 146, 252, 255), outline=(25, 86, 185, 255))
    d.rectangle([SIZE[0] - 88, SIZE[1] - 34, SIZE[0], SIZE[1]], fill=(18, 144, 232, 255))
    return base.convert("RGB")


def build_aqua() -> Image.Image:
    base = cover_crop(ROOT / "packages/theme-aqua/aqua-wave.jpg", SIZE)
    # Brushed top menu bar
    top_bar = vgrad(SIZE[0], 24, (248, 250, 253, 235), (218, 224, 232, 235))
    base.alpha_composite(top_bar, (0, 0))
    d = ImageDraw.Draw(base)
    d.line([(0, 24), (SIZE[0], 24)], fill=(155, 165, 180, 220), width=1)
    for i in range(4):
        d.rounded_rectangle([16 + i * 44, 8, 48 + i * 44, 16], radius=3, fill=(45, 55, 72, 175))

    wx0, wy0, wx1, wy1 = 112, 46, 608, 294
    add_shadow(base, (wx0, wy0, wx1, wy1), 10, alpha=105, blur=14)
    d = ImageDraw.Draw(base)
    d.rounded_rectangle([wx0, wy0, wx1, wy1], radius=10, fill=(247, 249, 252, 255), outline=(168, 178, 192, 255), width=1)
    tbar = vgrad(wx1 - wx0 - 2, 28, (238, 241, 246, 255), (206, 212, 221, 255))
    tbar.putalpha(round_mask((wx1 - wx0 - 2, 28), 9, (True, True, False, False)))
    base.alpha_composite(tbar, (wx0 + 1, wy0 + 1))
    d = ImageDraw.Draw(base)
    for i, c in enumerate([(255, 95, 87, 255), (254, 188, 46, 255), (40, 200, 64, 255)]):
        ox = wx0 + 14 + i * 18
        d.ellipse([ox, wy0 + 9, ox + 11, wy0 + 20], fill=c)
    mid = (wx0 + wx1) // 2
    d.rounded_rectangle([mid - 55, wy0 + 10, mid + 55, wy0 + 19], radius=4, fill=(60, 72, 88, 190))
    draw_mock_content(
        d,
        (wx0 + 2, wy0 + 30, wx1 - 2, wy1 - 2),
        (232, 238, 246, 255),
        (255, 255, 255, 255),
        (208, 216, 226, 255),
        (14, 165, 233, 255),
        (30, 41, 59, 210),
        (148, 163, 184, 175),
    )

    # Bottom Aqua crystal dock strip
    dock_x0, dock_y0, dock_x1, dock_y1 = 180, 314, 540, 352
    dock = Image.new("RGBA", (dock_x1 - dock_x0, dock_y1 - dock_y0), (240, 246, 255, 170))
    dock.putalpha(round_mask(dock.size, 12))
    base.alpha_composite(dock, (dock_x0, dock_y0))
    d = ImageDraw.Draw(base)
    d.rounded_rectangle([dock_x0, dock_y0, dock_x1, dock_y1], radius=12, outline=(255, 255, 255, 190), width=1)
    colors = [
        (56, 189, 248, 255),
        (59, 130, 246, 255),
        (16, 185, 129, 255),
        (99, 102, 241, 255),
        (249, 115, 22, 255),
        (100, 116, 139, 255),
    ]
    for idx, c in enumerate(colors):
        ix = dock_x0 + 22 + idx * 56
        d.rounded_rectangle([ix, dock_y0 + 7, ix + 26, dock_y0 + 31], radius=6, fill=c, outline=(255, 255, 255, 160))
    return base.convert("RGB")


def build_ambiance() -> Image.Image:
    base = cover_crop(ROOT / "packages/theme-ambiance/ambiance-wave.jpg", SIZE)
    # Warm charcoal top panel & slim left dock
    d = ImageDraw.Draw(base)
    d.rectangle([0, 0, SIZE[0], 24], fill=(44, 42, 40, 245))
    d.rectangle([0, 24, 48, SIZE[1]], fill=(38, 35, 34, 215))
    d.line([(48, 24), (48, SIZE[1])], fill=(255, 255, 255, 35), width=1)
    for i, c in enumerate([(224, 82, 34, 255), (59, 130, 246, 255), (245, 158, 11, 255), (16, 185, 129, 255)]):
        yy = 38 + i * 40
        d.rounded_rectangle([10, yy, 38, yy + 28], radius=7, fill=c)

    wx0, wy0, wx1, wy1 = 104, 46, 632, 316
    add_shadow(base, (wx0, wy0, wx1, wy1), 9, alpha=125, blur=14)
    d = ImageDraw.Draw(base)
    d.rounded_rectangle([wx0, wy0, wx1, wy1], radius=9, fill=(246, 244, 240, 255), outline=(62, 58, 55, 255), width=1)
    tbar = vgrad(wx1 - wx0, 28, (72, 68, 64, 255), (52, 48, 45, 255))
    tbar.putalpha(round_mask((wx1 - wx0, 28), 9, (True, True, False, False)))
    base.alpha_composite(tbar, (wx0, wy0))
    d = ImageDraw.Draw(base)
    for i, c in enumerate([(233, 84, 32, 255), (132, 126, 119, 255), (132, 126, 119, 255)]):
        ox = wx0 + 14 + i * 18
        d.ellipse([ox, wy0 + 9, ox + 11, wy0 + 20], fill=c)
    d.rounded_rectangle([wx0 + 76, wy0 + 10, wx0 + 210, wy0 + 19], radius=4, fill=(238, 232, 224, 220))
    draw_mock_content(
        d,
        (wx0 + 2, wy0 + 30, wx1 - 2, wy1 - 2),
        (237, 233, 227, 255),
        (255, 255, 255, 255),
        (214, 208, 200, 255),
        (233, 84, 32, 255),
        (48, 44, 40, 220),
        (158, 150, 142, 180),
    )
    return base.convert("RGB")


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    builders = {
        "theme-winxp.jpg": build_winxp,
        "theme-aqua.jpg": build_aqua,
        "theme-ambiance.jpg": build_ambiance,
    }
    for name, fn in builders.items():
        out = OUT_DIR / name
        im = fn()
        im.save(out, format="JPEG", quality=86, optimize=True, progressive=False)
        print(f"wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
