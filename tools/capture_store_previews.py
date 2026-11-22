#!/usr/bin/env python3
"""Capture real SPUI/Xvfb screenshots and compose 3D angled App Store showcases."""

import json
import os
import pathlib
import select
import shutil
import subprocess
import sys
import tempfile
import time
from PIL import Image, ImageDraw, ImageEnhance, ImageFilter

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
OUT_DIR = BUILD / "previews"
MUSL_REL = ROOT / "target/x86_64-unknown-linux-musl/release"
HOST_REL = ROOT / "target/release"

SCREEN_W = 1280
SCREEN_H = 720
CANVAS_W = 1920  # 2x supersampled composition canvas
CANVAS_H = 1080
PREVIEW_SIZE = (960, 540)


def check_bins() -> dict[str, pathlib.Path]:
    required = {
        "spui": MUSL_REL / "spui",
        "uxconfd": MUSL_REL / "uxconfd",
        "uxconf-query": MUSL_REL / "uxconf-query",
        "space-editor": MUSL_REL / "space-editor",
        "space-store": MUSL_REL / "space-store",
        "spk": HOST_REL / "spk",
        "spk-host": HOST_REL / "spk-host",
    }
    for name, path in required.items():
        if not path.is_file():
            raise SystemExit(f"missing required binary `{name}` at {path}")
    for tool in ("Xvfb", "import"):
        if shutil.which(tool) is None:
            raise SystemExit(f"missing host X11 tool `{tool}`")
    return required


def pick_display() -> int:
    for d in range(95, 110):
        sock = pathlib.Path(f"/tmp/.X11-unix/X{d}")
        if not sock.exists():
            return d
    raise SystemExit("no free Xvfb display (:95..:109)")


def wait_for_x(display: int, proc: subprocess.Popen[bytes]) -> None:
    sock = pathlib.Path(f"/tmp/.X11-unix/X{display}")
    deadline = time.time() + 8.0
    while time.time() < deadline:
        if proc.poll() is not None:
            raise SystemExit(f"Xvfb :{display} exited early ({proc.returncode})")
        if sock.exists():
            time.sleep(0.2)
            return
        time.sleep(0.05)
    raise SystemExit(f"timed out waiting for Xvfb :{display}")


def stop_proc(proc: subprocess.Popen[bytes] | None) -> None:
    if proc is None or proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=3.0)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=2.0)


def wait_for_ready(proc: subprocess.Popen[bytes], marker: bytes, timeout: float = 6.0) -> None:
    if proc.stderr is None:
        time.sleep(1.5)
        return
    fd = proc.stderr.fileno()
    buf = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            break
        r, _, _ = select.select([fd], [], [], 0.1)
        if fd in r:
            chunk = os.read(fd, 4096)
            if not chunk:
                break
            buf += chunk
            if marker in buf:
                return


def set_conf(
    bins: dict[str, pathlib.Path],
    env: dict[str, str],
    props: list[tuple[str, str, str, str]],
) -> None:
    q = str(bins["uxconf-query"])
    for ch, prop, typ, val in props:
        subprocess.run([q, "-c", ch, "-p", prop, "-t", typ, "-s", val], env=env, check=True)


THEMES: dict[str, list[tuple[str, str, str, str]]] = {
    "default": [
        ("appearance", "/theme/id", "string", "spui"),
        ("appearance", "/theme/style", "string", "spui"),
        ("appearance", "/theme/mode", "string", "light"),
        ("appearance", "/theme/accent", "string", "#2563eb"),
        ("desktop", "/backdrop/wallpaper", "string", "alpine-peaks.jpg"),
        ("desktop", "/backdrop/style", "string", "cover"),
        ("desktop", "/backdrop/solid-color", "string", "#1e293b"),
        ("panel", "/position", "string", "bottom"),
        ("panel", "/size", "int", "44"),
        ("panel", "/start-fullscreen", "bool", "false"),
        ("wm", "/button-layout", "string", "right"),
        ("wm", "/placement", "string", "center"),
    ],
    "winxp": [
        ("appearance", "/theme/id", "string", "winxp"),
        ("appearance", "/theme/style", "string", "spui"),
        ("appearance", "/theme/mode", "string", "light"),
        ("appearance", "/theme/accent", "string", "#0054e3"),
        ("desktop", "/backdrop/wallpaper", "string", "winxp-bliss.jpg"),
        ("desktop", "/backdrop/style", "string", "cover"),
        ("desktop", "/backdrop/solid-color", "string", "#3a6ea5"),
        ("panel", "/position", "string", "bottom"),
        ("panel", "/size", "int", "36"),
        ("panel", "/start-fullscreen", "bool", "false"),
        ("wm", "/button-layout", "string", "right"),
        ("wm", "/placement", "string", "center"),
    ],
    "aqua": [
        ("appearance", "/theme/id", "string", "aqua"),
        ("appearance", "/theme/style", "string", "spui"),
        ("appearance", "/theme/mode", "string", "light"),
        ("appearance", "/theme/accent", "string", "#1b74e4"),
        ("desktop", "/backdrop/wallpaper", "string", "aqua-wave.jpg"),
        ("desktop", "/backdrop/style", "string", "cover"),
        ("desktop", "/backdrop/solid-color", "string", "#1f5fa8"),
        ("panel", "/position", "string", "top"),
        ("panel", "/size", "int", "28"),
        ("panel", "/start-fullscreen", "bool", "false"),
        ("wm", "/button-layout", "string", "left"),
        ("wm", "/placement", "string", "center"),
    ],
    "ambiance": [
        ("appearance", "/theme/id", "string", "ambiance"),
        ("appearance", "/theme/style", "string", "spui"),
        ("appearance", "/theme/mode", "string", "light"),
        ("appearance", "/theme/accent", "string", "#dd4814"),
        ("desktop", "/backdrop/wallpaper", "string", "ambiance-wave.jpg"),
        ("desktop", "/backdrop/style", "string", "cover"),
        ("desktop", "/backdrop/solid-color", "string", "#4b1f3b"),
        ("panel", "/position", "string", "top"),
        ("panel", "/size", "int", "28"),
        ("panel", "/start-fullscreen", "bool", "false"),
        ("wm", "/button-layout", "string", "left"),
        ("wm", "/placement", "string", "center"),
    ],
}

# Curated atmospheric backdrop palettes:
# (bg_top_left, bg_bottom_right, orb1_rgb, orb2_rgb, glow_shadow_rgb)
PALETTES: dict[str, tuple[tuple[int, int, int], tuple[int, int, int], tuple[int, int, int], tuple[int, int, int], tuple[int, int, int]]] = {
    "theme-winxp": ((11, 24, 58), (16, 62, 134), (56, 189, 248), (34, 197, 94), (6, 18, 46)),
    "theme-aqua": ((10, 27, 52), (15, 84, 142), (125, 211, 252), (56, 189, 248), (8, 28, 58)),
    "theme-ambiance": ((52, 26, 48), (118, 46, 36), (251, 146, 60), (236, 72, 153), (24, 10, 22)),
    "space-editor": ((12, 19, 36), (24, 48, 118), (96, 165, 250), (56, 189, 248), (8, 16, 42)),
    "space-store": ((13, 18, 38), (38, 36, 104), (129, 140, 248), (56, 189, 248), (12, 14, 44)),
    "spime": ((8, 30, 48), (10, 82, 122), (125, 211, 252), (45, 212, 191), (6, 20, 34)),
}


def _xlib() -> tuple[ctypes.CDLL, ctypes.CDLL, ctypes.c_void_p]:
    import ctypes as _c

    x11 = _c.CDLL("libX11.so.6")
    xtst = _c.CDLL("libXtst.so.6")
    x11.XOpenDisplay.restype = _c.c_void_p
    x11.XStringToKeysym.restype = _c.c_ulong
    x11.XKeysymToKeycode.restype = _c.c_ubyte
    display = os.environ.get("DISPLAY", ":95").encode()
    dpy = x11.XOpenDisplay(display)
    if not dpy:
        raise SystemExit(f"cannot open display {display!r}")
    return x11, xtst, dpy


def xfocus_top() -> None:
    """Give the most recently mapped top-level window the input focus.

    The capture runs without a window manager, so nothing focuses the app
    window and XTEST keystrokes would go to the root window.
    """
    import ctypes as _c

    x11, _xtst, dpy = _xlib()
    root = x11.XDefaultRootWindow(_c.c_void_p(dpy))
    root_ret = _c.c_ulong()
    parent_ret = _c.c_ulong()
    children = _c.POINTER(_c.c_ulong)()
    count = _c.c_uint()
    if not x11.XQueryTree(
        _c.c_void_p(dpy), _c.c_ulong(root), _c.byref(root_ret),
        _c.byref(parent_ret), _c.byref(children), _c.byref(count),
    ):
        return
    if count.value:
        x11.XSetInputFocus(_c.c_void_p(dpy), _c.c_ulong(children[count.value - 1]), 2, 0)
        x11.XFlush(_c.c_void_p(dpy))
    time.sleep(0.2)


def xtype(text: str) -> None:
    """Type ASCII into the focused window through the XTEST extension."""
    import ctypes as _c

    x11, xtst, dpy = _xlib()
    for ch in text:
        keysym = x11.XStringToKeysym(ch.encode())
        if keysym == 0:
            continue
        keycode = x11.XKeysymToKeycode(_c.c_void_p(dpy), keysym)
        xtst.XTestFakeKeyEvent(_c.c_void_p(dpy), keycode, 1, 0)
        xtst.XTestFakeKeyEvent(_c.c_void_p(dpy), keycode, 0, 0)
        x11.XFlush(_c.c_void_p(dpy))
        time.sleep(0.06)
    time.sleep(0.5)


def xclick(x: int, y: int) -> None:
    import ctypes as _c

    x11, xtst, dpy = _xlib()
    xtst.XTestFakeMotionEvent(_c.c_void_p(dpy), -1, x, y, 0)
    x11.XFlush(_c.c_void_p(dpy))
    time.sleep(0.15)
    xtst.XTestFakeButtonEvent(_c.c_void_p(dpy), 1, 1, 0)
    xtst.XTestFakeButtonEvent(_c.c_void_p(dpy), 1, 0, 0)
    x11.XFlush(_c.c_void_p(dpy))
    time.sleep(0.2)


def start_spimed(tmp: pathlib.Path, base_env: dict[str, str]) -> subprocess.Popen[bytes]:
    spimed = BUILD / "packages/spime/usr/bin/spimed"
    staged = BUILD / "packages/spime/usr/share/spime"
    if not spimed.is_file() or not (staged / "spime.dict").is_file():
        raise SystemExit(f"missing staged spimed under {BUILD}/packages/spime; run `make packages` first")
    env = dict(base_env)
    env["SPIME_DICT_PATH"] = str(staged / "spime.dict")
    env["SPIME_LM_PATH"] = str(staged / "spime.lm")
    proc = subprocess.Popen(
        [str(spimed)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.7)
    return proc


def capture_raw(
    bins: dict[str, pathlib.Path],
    display: int,
    base_env: dict[str, str],
    tmp: pathlib.Path,
    theme_key: str,
    app_cmd: list[str],
    raw_name: str,
    wait_sec: float = 1.8,
    ime: bool = False,
    type_text: str | None = None,
    click_at: tuple[int, int] | None = None,
) -> pathlib.Path:
    set_conf(bins, base_env, THEMES[theme_key])
    shell_proc = subprocess.Popen(
        [str(bins["spui"]), "shell"],
        env=base_env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    app_proc = None
    ime_proc = None
    out_png = tmp / raw_name
    try:
        wait_for_ready(shell_proc, b"spui: desktop ready", timeout=6.0)
        time.sleep(0.4)
        if ime:
            ime_proc = start_spimed(tmp, base_env)
        app_proc = subprocess.Popen(
            app_cmd,
            env=base_env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        time.sleep(wait_sec)
        if ime:
            xfocus_top()
        if click_at is not None:
            xclick(*click_at)
        if type_text:
            xtype(type_text)
            time.sleep(0.4)
        if shell_proc.poll() is not None:
            err = shell_proc.stderr.read().decode(errors="replace") if shell_proc.stderr else ""
            raise SystemExit(f"spui shell exited during {raw_name} (rc={shell_proc.returncode}):\n{err}")
        if app_proc.poll() is not None:
            err = app_proc.stderr.read().decode(errors="replace") if app_proc.stderr else ""
            raise SystemExit(f"app {app_cmd} exited during {raw_name} (rc={app_proc.returncode}):\n{err}")
        subprocess.run(
            ["import", "-display", f":{display}", "-window", "root", str(out_png)],
            check=True,
        )
        return out_png
    finally:
        stop_proc(app_proc)
        if ime_proc is not None:
            stop_proc(ime_proc)
        stop_proc(shell_proc)
        time.sleep(0.2)


def solve_perspective_coeffs(
    dst_pts: list[tuple[float, float]],
    src_pts: list[tuple[float, float]],
) -> tuple[float, ...]:
    """Solve 8-coefficient PIL PERSPECTIVE homography mapping dst -> src."""
    matrix: list[list[float]] = []
    rhs: list[float] = []
    for (xd, yd), (xs, ys) in zip(dst_pts, src_pts):
        matrix.append([xd, yd, 1.0, 0.0, 0.0, 0.0, -xs * xd, -xs * yd])
        rhs.append(xs)
        matrix.append([0.0, 0.0, 0.0, xd, yd, 1.0, -ys * xd, -ys * yd])
        rhs.append(ys)

    n = 8
    aug = [matrix[r] + [rhs[r]] for r in range(n)]
    for col in range(n):
        piv = max(range(col, n), key=lambda r: abs(aug[r][col]))
        aug[col], aug[piv] = aug[piv], aug[col]
        div = aug[col][col]
        if abs(div) < 1e-12:
            raise ValueError("singular perspective transform")
        for j in range(col, n + 1):
            aug[col][j] /= div
        for r in range(n):
            if r != col and aug[r][col] != 0.0:
                factor = aug[r][col]
                for j in range(col, n + 1):
                    aug[r][j] -= factor * aug[col][j]
    return tuple(aug[r][n] for r in range(n))


def build_backdrop(palette_key: str, flip_glows: bool = False) -> Image.Image:
    c0, c1, orb1, orb2, _ = PALETTES[palette_key]
    w, h = CANVAS_W, CANVAS_H
    # Fast low-res gradient upscaled with BICUBIC gives silky banding-free mesh backdrop.
    gw, gh = 160, 90
    base = Image.new("RGBA", (gw, gh))
    px = base.load()
    for y in range(gh):
        vy = y / (gh - 1)
        for x in range(gw):
            vx = x / (gw - 1)
            t = 0.62 * vx + 0.38 * vy
            r = round(c0[0] + (c1[0] - c0[0]) * t)
            g = round(c0[1] + (c1[1] - c0[1]) * t)
            b = round(c0[2] + (c1[2] - c0[2]) * t)
            px[x, y] = (r, g, b, 255)
    canvas = base.resize((w, h), Image.Resampling.BICUBIC)

    # Ambient radial color glows (no text-washing diagonal glare sweep or dot noise)
    glow_layer = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow_layer)
    if not flip_glows:
        gd.ellipse([int(w * 0.56), -int(h * 0.28), int(w * 1.18), int(h * 0.65)], fill=(*orb1, 105))
        gd.ellipse([-int(w * 0.16), int(h * 0.42), int(w * 0.48), int(h * 1.24)], fill=(*orb2, 82))
    else:
        gd.ellipse([-int(w * 0.14), -int(h * 0.24), int(w * 0.46), int(h * 0.62)], fill=(*orb2, 98))
        gd.ellipse([int(w * 0.52), int(h * 0.40), int(w * 1.14), int(h * 1.22)], fill=(*orb1, 90))
    glow_layer = glow_layer.filter(ImageFilter.GaussianBlur(150))
    canvas.alpha_composite(glow_layer)
    return canvas


def prepare_framed_card(
    src_path: pathlib.Path,
    crop_box: tuple[int, int, int, int] | None,
    target_w: int,
    radius: int = 24,
    dim_factor: float = 1.0,
) -> Image.Image:
    im = Image.open(src_path).convert("RGBA")
    if crop_box is not None:
        im = im.crop(crop_box)
    aspect = im.height / max(1, im.width)
    target_h = max(120, round(target_w * aspect))
    im = im.resize((target_w, target_h), Image.Resampling.LANCZOS)
    im = im.filter(ImageFilter.UnsharpMask(radius=1.0, percent=115, threshold=2))
    if dim_factor < 0.999:
        im = ImageEnhance.Brightness(im).enhance(dim_factor)

    # Smooth rounded alpha mask (supersampled 2x internally for silky edges)
    mw, mh = target_w * 2, target_h * 2
    mask_2x = Image.new("L", (mw, mh), 0)
    md = ImageDraw.Draw(mask_2x)
    md.rounded_rectangle([0, 0, mw - 1, mh - 1], radius=radius * 2, fill=255)
    mask = mask_2x.resize((target_w, target_h), Image.Resampling.LANCZOS)
    im.putalpha(mask)

    # Crisp rim highlight around rounded bezel (no diagonal sheen covering UI text)
    border = Image.new("RGBA", (target_w, target_h), (0, 0, 0, 0))
    bd = ImageDraw.Draw(border)
    bd.rounded_rectangle(
        [1, 1, target_w - 2, target_h - 2],
        radius=radius,
        outline=(255, 255, 255, 148),
        width=2,
    )
    border.putalpha(
        Image.composite(border.getchannel("A"), Image.new("L", (target_w, target_h), 0), mask)
    )
    im.alpha_composite(border)
    return im


def place_perspective_card(
    canvas: Image.Image,
    card: Image.Image,
    quad: list[tuple[float, float]],
    shadow_color: tuple[int, int, int],
    soft_blur: float = 0.0,
) -> None:
    cw, ch = card.size
    src_pts = [(0.0, 0.0), (float(cw), 0.0), (float(cw), float(ch)), (0.0, float(ch))]
    coeffs = solve_perspective_coeffs(quad, src_pts)
    warped = card.transform(
        (CANVAS_W, CANVAS_H),
        Image.Transform.PERSPECTIVE,
        coeffs,
        Image.Resampling.BICUBIC,
    )
    if soft_blur > 0.05:
        warped = warped.filter(ImageFilter.GaussianBlur(soft_blur))

    alpha = warped.getchannel("A")
    # Layer 1: Wide diffused colored ambient drop shadow
    amb = Image.new("RGBA", (CANVAS_W, CANVAS_H), (*shadow_color, 200))
    amb.putalpha(alpha.point(lambda a: (a * 175) // 255))
    amb_shifted = Image.new("RGBA", (CANVAS_W, CANVAS_H), (0, 0, 0, 0))
    amb_shifted.paste(amb, (22, 46))
    amb_shifted = amb_shifted.filter(ImageFilter.GaussianBlur(48))
    canvas.alpha_composite(amb_shifted)

    # Layer 2: Tight crisp contact shadow right beneath floating card
    tight = Image.new("RGBA", (CANVAS_W, CANVAS_H), (5, 10, 20, 190))
    tight.putalpha(alpha.point(lambda a: (a * 165) // 255))
    tight_shifted = Image.new("RGBA", (CANVAS_W, CANVAS_H), (0, 0, 0, 0))
    tight_shifted.paste(tight, (8, 16))
    tight_shifted = tight_shifted.filter(ImageFilter.GaussianBlur(14))
    canvas.alpha_composite(tight_shifted)

    canvas.alpha_composite(warped)


def compose_pair(
    palette_key: str,
    shot1_path: pathlib.Path,
    shot2_path: pathlib.Path,
    crop_box: tuple[int, int, int, int] | None,
    out1_name: str,
    out2_name: str,
) -> None:
    shadow_rgb = PALETTES[palette_key][4]

    # --- Primary banner (`*.jpg`): Refined low-angle 3D perspective for high text readability ---
    c1 = build_backdrop(palette_key, flip_glows=False)
    back2 = prepare_framed_card(shot2_path, crop_box, target_w=1060, radius=20, dim_factor=0.78)
    place_perspective_card(
        c1,
        back2,
        quad=[(816, 54), (1848, 86), (1830, 720), (832, 694)],
        shadow_color=shadow_rgb,
        soft_blur=0.6,
    )
    front1 = prepare_framed_card(shot1_path, crop_box, target_w=1520, radius=22, dim_factor=1.0)
    place_perspective_card(
        c1,
        front1,
        quad=[(84, 116), (1588, 82), (1566, 996), (106, 964)],
        shadow_color=shadow_rgb,
        soft_blur=0.0,
    )
    save_final_jpeg(c1, OUT_DIR / out1_name)

    # --- Secondary gallery shot (`*-2.jpg`): Complementary gentle tilt with Shot2 upfront ---
    c2 = build_backdrop(palette_key, flip_glows=True)
    back1 = prepare_framed_card(shot1_path, crop_box, target_w=1040, radius=20, dim_factor=0.78)
    place_perspective_card(
        c2,
        back1,
        quad=[(72, 288), (1090, 258), (1076, 904), (90, 932)],
        shadow_color=shadow_rgb,
        soft_blur=0.6,
    )
    front2 = prepare_framed_card(shot2_path, crop_box, target_w=1520, radius=22, dim_factor=1.0)
    place_perspective_card(
        c2,
        front2,
        quad=[(328, 82), (1832, 116), (1810, 964), (350, 996)],
        shadow_color=shadow_rgb,
        soft_blur=0.0,
    )
    save_final_jpeg(c2, OUT_DIR / out2_name)


def save_final_jpeg(canvas: Image.Image, out_jpg: pathlib.Path) -> None:
    out_jpg.parent.mkdir(parents=True, exist_ok=True)
    rgb = canvas.convert("RGB").resize(PREVIEW_SIZE, Image.Resampling.LANCZOS)
    rgb = rgb.filter(ImageFilter.UnsharpMask(radius=0.85, percent=110, threshold=2))
    rgb.save(out_jpg, format="JPEG", quality=93, optimize=True, progressive=False)
    size = out_jpg.stat().st_size
    if size <= 15_000:
        raise SystemExit(f"captured preview {out_jpg} unexpectedly small ({size} bytes)")
    print(f"composed {out_jpg.relative_to(ROOT)} ({size} bytes)")


def centered_window_crop(win_w: int, win_h: int, panel_h: int = 44) -> tuple[int, int, int, int]:
    usable_h = SCREEN_H - panel_h
    wx = max(84, (SCREEN_W - win_w) // 2)
    wy = max(8, (usable_h - win_h) // 2)
    return (wx, wy, wx + win_w, wy + win_h)


def setup_sysroot_and_assets(
    tmp: pathlib.Path, bins: dict[str, pathlib.Path]
) -> tuple[pathlib.Path, pathlib.Path, pathlib.Path]:
    sysroot = tmp / "sysroot"
    stage = BUILD / "rootfs/stage"
    if not (stage / "usr/share/fonts/noto").is_dir():
        raise SystemExit(f"missing staged noto fonts under {stage}/usr/share/fonts/noto")

    (sysroot / "usr/share").mkdir(parents=True, exist_ok=True)
    (sysroot / "usr/bin").mkdir(parents=True, exist_ok=True)
    (sysroot / "usr/share/applications").mkdir(parents=True, exist_ok=True)

    os.symlink(stage / "usr/share/fonts", sysroot / "usr/share/fonts")
    os.symlink(stage / "usr/share/spui", sysroot / "usr/share/spui")
    os.symlink(bins["space-store"], sysroot / "usr/bin/space-store")
    os.symlink(bins["space-editor"], sysroot / "usr/bin/space-editor")
    shutil.copy2(
        ROOT / "packages/space-store/space-store.desktop",
        sysroot / "usr/share/applications/space-store.desktop",
    )
    shutil.copy2(
        ROOT / "packages/space-editor/space-editor.desktop",
        sysroot / "usr/share/applications/space-editor.desktop",
    )

    themes_dir = tmp / "themes"
    themes_dir.mkdir(parents=True, exist_ok=True)
    for name in ("winxp", "aqua", "ambiance"):
        staged_theme = BUILD / f"packages/theme-{name}/usr/share/spui/themes/{name}"
        if not staged_theme.is_dir():
            raise SystemExit(f"missing staged theme {staged_theme}; run `make packages` first")
        os.symlink(staged_theme, themes_dir / name)

    wallpapers_dir = tmp / "wallpapers"
    wallpapers_dir.mkdir(parents=True, exist_ok=True)
    for item in (stage / "usr/share/spui/wallpapers").iterdir():
        if item.is_file():
            os.symlink(item, wallpapers_dir / item.name)
    for pkg, fname in [
        ("theme-winxp", "winxp-bliss.jpg"),
        ("theme-aqua", "aqua-wave.jpg"),
        ("theme-ambiance", "ambiance-wave.jpg"),
    ]:
        src = ROOT / "packages" / pkg / fname
        if src.is_file() and not (wallpapers_dir / fname).exists():
            os.symlink(src, wallpapers_dir / fname)

    return sysroot, themes_dir, wallpapers_dir


def prepare_local_store_env(
    tmp: pathlib.Path, bins: dict[str, pathlib.Path], base_env: dict[str, str]
) -> dict[str, str]:
    repo_src = BUILD / "repo/x86_64"
    local_repo = tmp / "local-repo"
    local_repo.mkdir(parents=True, exist_ok=True)
    for spk in repo_src.glob("*.spk"):
        shutil.copy2(spk, local_repo / spk.name)
    if (ROOT / "packages/store.in.json").is_file():
        shutil.copy2(ROOT / "packages/store.in.json", local_repo / "store.in.json")
    prev_dst = local_repo / "previews"
    prev_dst.mkdir(parents=True, exist_ok=True)
    for jpg in OUT_DIR.glob("*.jpg"):
        shutil.copy2(jpg, prev_dst / jpg.name)

    key = BUILD / "keys/repo.key"
    pub = BUILD / "keys/repo.pub"
    if not key.is_file() or not pub.is_file():
        raise SystemExit(f"missing repo keys in {BUILD}/keys; run `make repo` first")

    subprocess.run(
        [
            str(bins["spk-host"]),
            "index",
            "--repo-dir",
            str(local_repo),
            "--repo",
            "spaceos-main",
            "--arch",
            "x86_64",
            "--key",
            str(key),
        ],
        check=True,
    )

    spk_root = tmp / "spk-root"
    (spk_root / "etc/spk/trusted.d").mkdir(parents=True, exist_ok=True)
    (spk_root / "var/lib/spk").mkdir(parents=True, exist_ok=True)
    (spk_root / "var/cache/spk").mkdir(parents=True, exist_ok=True)
    pub_obj = json.loads(pub.read_text())
    shutil.copy2(pub, spk_root / "etc/spk/trusted.d" / f"{pub_obj['key_id']}.pub")
    (spk_root / "etc/spk/trusted.json").write_text(
        json.dumps({"schema": 1, "keys": [pub_obj]}, indent=2) + "\n"
    )
    (spk_root / "var/lib/spk/repos.json").write_text(
        json.dumps(
            {
                "schema": 1,
                "repos": [
                    {
                        "name": "spaceos-main",
                        "arch": "x86_64",
                        "urls": [str(local_repo)],
                        "priority": 10,
                        "key_id": pub_obj["key_id"],
                        "last_sync": int(time.time()),
                        "enabled": True,
                    }
                ],
            },
            indent=2,
        )
        + "\n"
    )
    subprocess.run(
        [str(bins["spk"]), "--root", str(spk_root), "--noconfirm", "sync"],
        check=True,
    )

    store_env = dict(base_env)
    store_env["SPK_BIN"] = str(bins["spk"])
    store_env["SPK_ROOT"] = str(spk_root)
    store_env["XDG_CACHE_HOME"] = str(tmp / "store-cache")
    return store_env


def main() -> int:
    bins = check_bins()
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for stale in OUT_DIR.glob("*.jpg"):
        # Font specimen cards belong to build_font_previews.py; keep them.
        if stale.name.startswith("font-"):
            continue
        stale.unlink()

    display = pick_display()
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="spui-capture-"))
    xvfb = None
    uxconfd = None

    try:
        sysroot, themes_dir, wallpapers_dir = setup_sysroot_and_assets(tmp, bins)
        ux_user = tmp / "ux-user"
        ux_sys = tmp / "ux-sys"
        home_dir = tmp / "home"
        for d in (ux_user, ux_sys, home_dir, home_dir / "Documents"):
            d.mkdir(parents=True, exist_ok=True)

        env = dict(os.environ)
        env.update(
            {
                "DISPLAY": f":{display}",
                "HOME": str(home_dir),
                "SPUI_SYSROOT": str(sysroot),
                "SPUI_THEME_DIR": str(themes_dir),
                "SPUI_WALLPAPER_DIR": str(wallpapers_dir),
                "UXCONF_SOCKET": str(tmp / "uxconfd.sock"),
                "UXCONF_USER_DIR": str(ux_user),
                "UXCONF_SYSTEM_DIR": str(ux_sys),
                "SPIME_NO_AUTOSPAWN": "1",
                "SPIME_SOCKET": str(tmp / "spimed.sock"),
                "XMODIFIERS": "@im=spime",
            }
        )

        xvfb = subprocess.Popen(
            [
                "Xvfb",
                f":{display}",
                "-screen",
                "0",
                f"{SCREEN_W}x{SCREEN_H}x24",
                "-nolisten",
                "tcp",
                "-ac",
                "+extension",
                "RENDER",
                "+extension",
                "COMPOSITE",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        wait_for_x(display, xvfb)

        uxconfd = subprocess.Popen(
            [str(bins["uxconfd"])],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        time.sleep(0.3)

        # Pass 1: Theme packs + SpaceEditor (raw real captures -> 3D angled layered composition)
        p_winxp_1 = capture_raw(bins, display, env, tmp, "winxp", [str(bins["spui"]), "settings", "appearance"], "raw-winxp-1.png", 1.8)
        p_winxp_2 = capture_raw(bins, display, env, tmp, "winxp", [str(bins["space-editor"]), "--demo"], "raw-winxp-2.png", 1.8)
        compose_pair("theme-winxp", p_winxp_1, p_winxp_2, None, "theme-winxp.jpg", "theme-winxp-2.jpg")

        p_aqua_1 = capture_raw(bins, display, env, tmp, "aqua", [str(bins["spui"]), "settings", "appearance"], "raw-aqua-1.png", 1.8)
        p_aqua_2 = capture_raw(bins, display, env, tmp, "aqua", [str(bins["space-editor"]), "--demo"], "raw-aqua-2.png", 1.8)
        compose_pair("theme-aqua", p_aqua_1, p_aqua_2, None, "theme-aqua.jpg", "theme-aqua-2.jpg")

        p_amb_1 = capture_raw(bins, display, env, tmp, "ambiance", [str(bins["spui"]), "settings", "appearance"], "raw-amb-1.png", 1.8)
        p_amb_2 = capture_raw(bins, display, env, tmp, "ambiance", [str(bins["space-editor"]), "--demo"], "raw-amb-2.png", 1.8)
        compose_pair("theme-ambiance", p_amb_1, p_amb_2, None, "theme-ambiance.jpg", "theme-ambiance-2.jpg")

        p_ed_1 = capture_raw(bins, display, env, tmp, "default", [str(bins["space-editor"]), "--demo"], "raw-ed-1.png", 1.8)
        p_ed_2 = capture_raw(
            bins,
            display,
            env,
            tmp,
            "default",
            [str(bins["space-editor"]), "--demo", "--tab", "insert", "--find", "SpaceOS"],
            "raw-ed-2.png",
            1.8,
        )
        compose_pair(
            "space-editor",
            p_ed_1,
            p_ed_2,
            centered_window_crop(1040, 640),
            "space-editor.jpg",
            "space-editor-2.jpg",
        )

        # Pass 2: Space Store screenshots consuming all 8 real angled previews
        store_env = prepare_local_store_env(tmp, bins, env)
        p_st_1 = capture_raw(
            bins,
            display,
            store_env,
            tmp,
            "default",
            [str(bins["space-store"])],
            "raw-st-1.png",
            3.0,
        )
        p_st_2 = capture_raw(
            bins,
            display,
            store_env,
            tmp,
            "default",
            [str(bins["space-store"]), "--package", "space-editor"],
            "raw-st-2.png",
            3.0,
        )
        compose_pair(
            "space-store",
            p_st_1,
            p_st_2,
            centered_window_crop(1060, 648),
            "space-store.jpg",
            "space-store-2.jpg",
        )
        font_script = ROOT / "tools" / "build_font_previews.py"
        if font_script.is_file():
            # Keep the font specimen set in step with a capture run.
            subprocess.run([sys.executable, str(font_script)], check=False)
        print(f"composed {len(list(OUT_DIR.glob('*.jpg')))} angled previews into {OUT_DIR}")
        return 0
    finally:
        stop_proc(uxconfd)
        stop_proc(xvfb)
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
