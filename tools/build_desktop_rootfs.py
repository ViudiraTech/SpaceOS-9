#!/usr/bin/env python3
#
#       tools/build_desktop_rootfs.py
#       Assemble the ext4 desktop root image from Alpine and SpaceOS userland
#
#       2026/10/30 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Build build/rootfs/ext4.img.

Alpine packages supply the C userland (musl, BusyBox, Xorg and its libraries).
SpaceOS supplies init, session and desktop programs. Packages are fetched at
build time and verified against the repository index, so this repository never
contains third-party binaries. APKINDEX signatures are not re-verified here:
the index comes over HTTPS from the release mirror and pins each package by
its SHA-1 control checksum and size.
"""

import argparse
import base64
import hashlib
import io
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tarfile
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD = ROOT / "build" / "rootfs"
CACHE = ROOT / "build" / "downloads" / "alpine"
STAGE = BUILD / "stage"
IMAGE = BUILD / "ext4.img"
USERLAND = ROOT / "build" / "userland" / "bin"

ALPINE_RELEASE = "v3.24"
MIRROR = f"https://dl-cdn.alpinelinux.org/alpine/{ALPINE_RELEASE}"
REPOSITORIES = ("main", "community")

# Roots of the package closure. libudev-zero provides libudev without a daemon,
# so it replaces eudev-libs as the provider of so:libudev.so.1.
PACKAGES = [
    "alpine-baselayout-data",
    "musl",
    "busybox",
    "busybox-suid",
    "doas",
    "xorg-server",
    "xf86-input-libinput",
    "xf86-input-evdev",
    "xkeyboard-config",
    "xkbcomp",
    "setxkbmap",
    "font-misc-misc",
    "font-cursor-misc",
    "libudev-zero",
    "xterm",
    "xrandr",
    "xdpyinfo",
    "xset",
    "xsetroot",
    "xprop",
    "font-dejavu",
    "font-noto-cjk",
    "breeze-cursors",
    "tzdata",
    "file",
    "htop",
    "nano",
    "tree",
    "fastfetch",
]
WALLPAPERS = (
    ("alpine-peaks.jpg", "https://picsum.photos/id/29/1920/1200.jpg"),
    ("misty-pines.jpg", "https://picsum.photos/id/10/1920/1200.jpg"),
    ("coastal-horizon.jpg", "https://picsum.photos/id/16/1920/1200.jpg"),
    ("emerald-forest.jpg", "https://picsum.photos/id/28/1920/1200.jpg"),
)
BREEZE_ICONS = {
    "terminal": "usr/share/icons/breeze/apps/48/utilities-terminal.svg",
    "files": "usr/share/icons/breeze/apps/48/system-file-manager.svg",
    "editor": "usr/share/icons/breeze/apps/48/accessories-text-editor.svg",
    "settings": "usr/share/icons/breeze/apps/48/preferences-system.svg",
    "calculator": "usr/share/icons/breeze/apps/48/accessories-calculator.svg",
    "monitor": "usr/share/icons/breeze/apps/48/utilities-system-monitor.svg",
    "image-viewer": "usr/share/icons/breeze/apps/48/gwenview.svg",
    "help": "usr/share/icons/breeze/apps/48/help-browser.svg",
    "login": "usr/share/icons/breeze/preferences/32/preferences-system-login.svg",
    "notify": "usr/share/icons/breeze/preferences/32/preferences-desktop-notification-bell.svg",
    "appearance": "usr/share/icons/breeze/preferences/32/preferences-desktop-theme-global.svg",
    "wallpaper": "usr/share/icons/breeze/preferences/32/preferences-desktop-wallpaper.svg",
    "panel": "usr/share/icons/breeze/preferences/32/preferences-desktop-plasma.svg",
    "windows": "usr/share/icons/breeze/preferences/32/preferences-system-windows.svg",
    "display": "usr/share/icons/breeze/preferences/32/preferences-desktop-display.svg",
    "peripherals": "usr/share/icons/breeze/preferences/32/preferences-desktop-peripherals.svg",
    "applications": "usr/share/icons/breeze/preferences/32/preferences-desktop-default-applications.svg",
    "datetime": "usr/share/icons/breeze/preferences/32/preferences-system-time.svg",
    "about": "usr/share/icons/breeze/actions/32/help-about.svg",
    "folder": "usr/share/icons/breeze/places/32/folder.svg",
    "folder-large": "usr/share/icons/breeze/places/64/folder.svg",
    "user-home": "usr/share/icons/breeze/places/32/user-home.svg",
    "folder-root": "usr/share/icons/breeze/places/32/folder-root.svg",
    "folder-documents": "usr/share/icons/breeze/places/32/folder-documents.svg",
    "folder-temp": "usr/share/icons/breeze/places/32/folder-temp.svg",
    "folder-script": "usr/share/icons/breeze/places/32/folder-script.svg",
    "file-text": "usr/share/icons/breeze/mimetypes/32/text-plain.svg",
    "file-rust": "usr/share/icons/breeze/mimetypes/32/text-x-rust.svg",
    "file-c": "usr/share/icons/breeze/mimetypes/32/text-x-csrc.svg",
    "file-script": "usr/share/icons/breeze/mimetypes/32/text-x-script.svg",
    "file-exec": "usr/share/icons/breeze/mimetypes/32/application-x-executable.svg",
    "file-bin": "usr/share/icons/breeze/mimetypes/32/application-octet-stream.svg",
    "file-image": "usr/share/icons/breeze/mimetypes/32/image-x-generic.svg",
    "search": "usr/share/icons/breeze/actions/22/edit-find.svg",
    "save": "usr/share/icons/breeze/actions/22/document-save.svg",
    "new-file": "usr/share/icons/breeze/actions/22/document-new.svg",
    "new-folder": "usr/share/icons/breeze/actions/22/folder-new.svg",
    "refresh": "usr/share/icons/breeze/actions/22/view-refresh.svg",
    "go-up": "usr/share/icons/breeze/actions/22/go-up.svg",
    "go-back": "usr/share/icons/breeze/actions/22/go-previous.svg",
    "go-home": "usr/share/icons/breeze/actions/22/go-home.svg",
    "logout": "usr/share/icons/breeze/actions/22/system-log-out.svg",
    "shutdown": "usr/share/icons/breeze/actions/22/system-shutdown.svg",
    "bell": "usr/share/icons/breeze/actions/22/notification-empty.svg",
    "bell-active": "usr/share/icons/breeze/actions/22/notification-active.svg",
    "info": "usr/share/icons/breeze/actions/22/help-about.svg",
}
PROVIDER_PREFERENCE = {
    "so:libudev.so.1": "libudev-zero",
    "udev": "libudev-zero",
}
# Features the kernel driver supports. dir_index is off because modifying a
# hashed directory is not implemented; see src/vfs/ext4/mod.rs.
EXT4_FEATURES = (
    "has_journal,extent,huge_file,flex_bg,metadata_csum,metadata_csum_seed,64bit,"
    "dir_nlink,extra_isize,^orphan_file,^resize_inode,^dir_index"
)


def log(message: str) -> None:
    print(f"[rootfs] {message}", flush=True)


def fetch(url: str, path: pathlib.Path) -> None:
    if path.exists():
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".part")
    with urllib.request.urlopen(url, timeout=120) as response, open(temporary, "wb") as out:
        shutil.copyfileobj(response, out)
    temporary.rename(path)


def parse_index(data: bytes, repository: str) -> list[dict]:
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        text = archive.extractfile("APKINDEX").read().decode("utf-8", "replace")
    packages = []
    for block in text.split("\n\n"):
        record = {"repository": repository}
        for line in block.splitlines():
            if line[1:2] == ":":
                record[line[0]] = line[2:]
        if "P" in record:
            packages.append(record)
    return packages


def load_indexes() -> tuple[dict, dict]:
    packages: dict[str, dict] = {}
    providers: dict[str, list[str]] = {}
    for repository in REPOSITORIES:
        path = CACHE / f"APKINDEX-{ALPINE_RELEASE}-{repository}.tar.gz"
        fetch(f"{MIRROR}/{repository}/x86_64/APKINDEX.tar.gz", path)
        for record in parse_index(path.read_bytes(), repository):
            packages[record["P"]] = record
            providers.setdefault(record["P"], []).append(record["P"])
            for item in record.get("p", "").split():
                providers.setdefault(item.split("=")[0], []).append(record["P"])
    return packages, providers


def clean_dependency(name: str) -> str:
    for separator in "<>=~":
        name = name.split(separator)[0]
    return name


def resolve(packages: dict, providers: dict) -> dict[str, dict]:
    selected: dict[str, dict] = {}
    pending = list(PACKAGES)
    while pending:
        name = clean_dependency(pending.pop())
        if name.startswith("!"):
            continue
        preferred = PROVIDER_PREFERENCE.get(name)
        if preferred:
            target = preferred
        elif name in packages:
            target = name
        elif providers.get(name):
            target = sorted(providers[name])[0]
        else:
            raise SystemExit(f"unresolvable dependency: {name}")
        if target in selected:
            continue
        record = packages[target]
        selected[target] = record
        pending.extend(record.get("D", "").split())
    return selected


def download(record: dict) -> pathlib.Path:
    name = f"{record['P']}-{record['V']}.apk"
    path = CACHE / "packages" / name
    fetch(f"{MIRROR}/{record['repository']}/x86_64/{name}", path)
    size = int(record["S"])
    if path.stat().st_size != size:
        path.unlink()
        raise SystemExit(f"{name}: size mismatch")
    # C: is "Q1" + base64(SHA-1 of the control tar segment).
    expected = base64.b64decode(record["C"][2:])
    payload = path.read_bytes()
    segments = split_gzip_members(payload)
    control = segments[1] if len(segments) == 3 else segments[0]
    if hashlib.sha1(control).digest() != expected:
        path.unlink()
        raise SystemExit(f"{name}: control checksum mismatch")
    return path


def split_gzip_members(payload: bytes) -> list[bytes]:
    """Split an apk (concatenated gzip streams) into raw member bytes."""
    import zlib

    members = []
    offset = 0
    while offset < len(payload):
        decompressor = zlib.decompressobj(zlib.MAX_WBITS | 16)
        decompressor.decompress(payload[offset:])
        consumed = len(payload) - offset - len(decompressor.unused_data)
        members.append(payload[offset : offset + consumed])
        offset += consumed
    return members


def safe_member(member: tarfile.TarInfo) -> bool:
    name = member.name
    if name.startswith(".") and "/" not in name:
        return False
    if name.startswith("/") or ".." in pathlib.PurePosixPath(name).parts:
        raise SystemExit(f"unsafe archive member: {name}")
    if member.issym() or member.islnk():
        if member.islnk() and (member.linkname.startswith("/") or ".." in member.linkname):
            raise SystemExit(f"unsafe hard link: {name}")
    return True


def extract(path: pathlib.Path) -> None:
    payload = path.read_bytes()
    data = split_gzip_members(payload)[-1]
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive:
            if not safe_member(member):
                continue
            target = STAGE / member.name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                os.chmod(target, member.mode | 0o700)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.exists() or target.is_symlink():
                if target.is_dir() and not target.is_symlink():
                    continue
                target.unlink()
            if member.issym():
                os.symlink(member.linkname, target)
            elif member.islnk():
                os.link(STAGE / member.linkname, target)
            elif member.isfile():
                with archive.extractfile(member) as source, open(target, "wb") as out:
                    shutil.copyfileobj(source, out)
                os.chmod(target, (member.mode & 0o7777) | 0o600)
            # Device nodes come from devtmpfs at runtime.


def write(path: str, content: str, mode: int = 0o644) -> None:
    target = STAGE / path.lstrip("/")
    target.parent.mkdir(parents=True, exist_ok=True)
    if target.exists() or target.is_symlink():
        target.unlink()
    target.write_text(content)
    os.chmod(target, mode)


def install_binary(source: pathlib.Path, destination: str, mode: int = 0o755) -> None:
    target = STAGE / destination.lstrip("/")
    target.parent.mkdir(parents=True, exist_ok=True)
    if target.exists() or target.is_symlink():
        target.unlink()
    shutil.copy2(source, target)
    os.chmod(target, mode)


def busybox_links() -> None:
    """Replace apk's post-install trigger: every applet links to BusyBox."""
    binary = STAGE / "bin" / "busybox"
    if not binary.exists():
        return
    os.chmod(binary, 0o755)
    host_list = subprocess.run(
        ["strings", "-n", "2", str(binary)], check=True, capture_output=True, text=True
    ).stdout
    # Applet names are stored in a NUL separated table beginning at "[".
    known = (
        "ash sh cat cp mv rm ls ln mkdir rmdir touch echo printf grep sed awk head tail wc "
        "sort uniq cut tr tee find xargs ps kill sleep usleep uname id whoami date env true false "
        "clear reset stty tty hostname df du free mount umount dmesg top ping mknod chmod "
        "chown basename dirname readlink realpath which test expr seq sync vi more less "
        "gzip gunzip tar poweroff reboot halt base64 cmp dd md5sum sha256sum strings nproc diff "
        "login getty nologin su passwd vlock adduser addgroup chpasswd"
    ).split()
    suid = {"su", "passwd"}
    bbsuid = STAGE / "bin" / "bbsuid"
    for name in known:
        if name not in host_list and name not in ("sh", "ash"):
            continue
        link = STAGE / "bin" / name
        if link.exists() or link.is_symlink():
            continue
        if name in suid and bbsuid.exists():
            os.chmod(bbsuid, 0o4755)
            link.symlink_to("/bin/bbsuid")
        else:
            link.symlink_to("/bin/busybox")


def ensure_cached_package(pkg_name: str) -> pathlib.Path:
    matches = sorted((CACHE / "packages").glob(f"{pkg_name}-[0-9]*.apk"))
    if matches:
        return matches[-1]
    packages, _ = load_indexes()
    if pkg_name not in packages:
        raise SystemExit(f"package not found in Alpine index: {pkg_name}")
    return download(packages[pkg_name])


def resolve_tar_symlink(names: dict[str, tarfile.TarInfo], start: str) -> tarfile.TarInfo:
    cur = start
    for _ in range(12):
        member = names.get(cur)
        if member is None:
            raise SystemExit(f"missing breeze icon member: {start} (at {cur})")
        if not member.issym():
            return member
        joined = pathlib.PurePosixPath(cur).parent.joinpath(member.linkname)
        parts: list[str] = []
        for part in joined.parts:
            if part == "..":
                if parts:
                    parts.pop()
            elif part != ".":
                parts.append(part)
        cur = "/".join(parts)
    raise SystemExit(f"symlink loop in breeze-icons: {start}")


def install_spui_assets() -> None:
    if not (STAGE / "usr/share/icons/breeze_cursors/cursors/left_ptr").exists():
        log("installing breeze-cursors into stage")
        extract(ensure_cached_package("breeze-cursors"))

    icon_dir = STAGE / "usr/share/spui/icons/breeze"
    icon_dir.mkdir(parents=True, exist_ok=True)
    missing_icons = [k for k in BREEZE_ICONS if not (icon_dir / f"{k}.svg").exists()]
    if missing_icons:
        log(f"extracting {len(BREEZE_ICONS)} curated Breeze SVG icons")
        apk = ensure_cached_package("breeze-icons")
        data = split_gzip_members(apk.read_bytes())[-1]
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            names = {m.name: m for m in archive.getmembers()}
            for key, member_path in BREEZE_ICONS.items():
                resolved = resolve_tar_symlink(names, member_path)
                source = archive.extractfile(resolved)
                if source is None:
                    raise SystemExit(f"unreadable breeze icon: {member_path}")
                out_path = icon_dir / f"{key}.svg"
                out_path.write_bytes(source.read())
                os.chmod(out_path, 0o644)

    wallpaper_cache = ROOT / "build" / "downloads" / "wallpapers"
    wallpaper_cache.mkdir(parents=True, exist_ok=True)
    for filename, url in WALLPAPERS:
        cached = wallpaper_cache / filename
        if not cached.exists():
            log(f"fetching wallpaper {filename}")
            req = urllib.request.Request(url, headers={"User-Agent": "SpaceOS-Build/9.0"})
            temp = cached.with_suffix(".jpg.part")
            with urllib.request.urlopen(req, timeout=120) as resp, open(temp, "wb") as out:
                shutil.copyfileobj(resp, out)
            temp.rename(cached)
        install_binary(cached, f"/usr/share/spui/wallpapers/{filename}", 0o644)

    logo = ROOT / "docs" / "SpaceOS9.png"
    if logo.exists():
        install_binary(logo, "/usr/share/spui/logo.png", 0o644)


def generate_misc_font_index() -> None:
    misc_dir = STAGE / "usr/share/fonts/misc"
    if not misc_dir.is_dir():
        return
    mkfontscale = shutil.which("mkfontscale")
    mkfontdir = shutil.which("mkfontdir")
    if mkfontscale:
        subprocess.run([mkfontscale, str(misc_dir)], check=False)
    if mkfontdir:
        subprocess.run([mkfontdir, str(misc_dir)], check=False)
    fonts_dir = misc_dir / "fonts.dir"
    if not fonts_dir.exists() or fonts_dir.stat().st_size < 16:
        write(
            "/usr/share/fonts/misc/fonts.dir",
            "2\n"
            "cursor.pcf.gz cursor\n"
            "6x13-ISO8859-1.pcf.gz -misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-1\n",
        )
    write(
        "/usr/share/fonts/misc/fonts.alias",
        "fixed -misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-1\n"
        "cursor cursor\n",
    )


def configure() -> None:
    for directory in (
        "dev", "proc", "sys", "tmp", "run", "root", "home", "etc/X11/xorg.conf.d",
        "var/log", "var/log/ddm", "var/lib/xkb", "var/lib/ddm", "var/lib/spaceos",
        "etc/systemg", "etc/ddm", "usr/share/spui", "usr/share/xsessions", "etc/spui",
        "etc/uxconf/channels", "var/lib/uxconf/channels", "var/cache/fontconfig",
        "var/mail", "run/ddm", "run/user",
    ):
        (STAGE / directory).mkdir(parents=True, exist_ok=True)
    os.chmod(STAGE / "tmp", 0o1777)
    os.chmod(STAGE / "run" / "ddm", 0o711)
    os.chmod(STAGE / "run" / "user", 0o755)
    os.chmod(STAGE / "var" / "lib" / "ddm", 0o700)
    os.chmod(STAGE / "var" / "log" / "ddm", 0o755)
    os.chmod(STAGE / "home", 0o755)
    (STAGE / "var" / "db" / "doas").mkdir(parents=True, exist_ok=True)
    os.chmod(STAGE / "var" / "db" / "doas", 0o700)
    (STAGE / "etc" / "skel").mkdir(parents=True, exist_ok=True)
    if not (STAGE / "var" / "run").exists() and not (STAGE / "var" / "run").is_symlink():
        os.symlink("../run", STAGE / "var" / "run")
    write("/var/log/wtmp", "", 0o664)
    write("/var/log/btmp", "", 0o600)
    write("/var/log/lastlog", "", 0o644)
    busybox_links()

    for stale_probe in (
        "usr/bin/drmprobe",
        "usr/bin/drm-probe",
        "etc/spui/settings.conf",
        "etc/systemg/desktop.service",
        "home/user",
        "etc/systemg/oobe.service",
    ):
        probe_path = STAGE / stale_probe
        if probe_path.is_dir() and not probe_path.is_symlink():
            shutil.rmtree(probe_path)
        elif probe_path.exists() or probe_path.is_symlink():
            probe_path.unlink()

    uxconf_defaults = {
        "appearance": (
            '# uxconf channel: appearance\n'
            '/font/scale = int:100\n'
            '/theme/accent = string:"#3b82f6"\n'
            '/theme/mode = string:"dark"\n'
        ),
        "desktop": (
            '# uxconf channel: desktop\n'
            '/backdrop/solid-color = string:"#1e293b"\n'
            '/backdrop/style = string:"cover"\n'
            '/backdrop/wallpaper = string:"alpine-peaks.jpg"\n'
            '/icons/size = int:40\n'
            '/icons/visible = bool:true\n'
        ),
        "panel": (
            '# uxconf channel: panel\n'
            '/position = string:"bottom"\n'
            '/show-pinned = bool:true\n'
            '/size = int:46\n'
        ),
        "wm": (
            '# uxconf channel: wm\n'
            '/auto-raise = bool:false\n'
            '/button-layout = string:"right"\n'
            '/double-click-action = string:"maximize"\n'
            '/focus-mode = string:"click"\n'
            '/placement = string:"cascade"\n'
        ),
        "display": (
            '# uxconf channel: display\n'
            '/blank-timeout-sec = int:0\n'
            '/resolution = string:"1280x800"\n'
        ),
        "input": (
            '# uxconf channel: input\n'
            '/keyboard/repeat-delay-ms = int:400\n'
            '/keyboard/repeat-enabled = bool:true\n'
            '/keyboard/repeat-rate-hz = int:30\n'
            '/mouse/double-click-ms = int:380\n'
            '/mouse/left-handed = bool:false\n'
            '/mouse/natural-scroll = bool:false\n'
            '/mouse/speed = int:3\n'
        ),
        "terminal": (
            '# uxconf channel: terminal\n'
            '/cursor-style = string:"block"\n'
            '/font-size = int:14\n'
            '/scrollback-lines = int:1000\n'
        ),
        "files": (
            '# uxconf channel: files\n'
            '/folders-first = bool:true\n'
            '/show-hidden = bool:false\n'
            '/single-click-open = bool:false\n'
        ),
        "editor": (
            '# uxconf channel: editor\n'
            '/font-size = int:14\n'
            '/highlight-active-line = bool:true\n'
            '/show-line-numbers = bool:true\n'
            '/tab-width = int:4\n'
        ),
        "datetime": (
            '# uxconf channel: datetime\n'
            '/clock-24h = bool:true\n'
            '/clock-seconds = bool:false\n'
            '/clock-show-date = bool:true\n'
            '/timezone = string:"UTC"\n'
        ),
    }
    for channel_name, channel_body in uxconf_defaults.items():
        write(f"/etc/uxconf/channels/{channel_name}.conf", channel_body)

    write("/etc/hostname", "spaceos\n")
    write(
        "/etc/passwd",
        "root:x:0:0:root:/root:/bin/sh\n"
        "ddm:x:990:990:SpaceOS Display Manager:/var/lib/ddm:/bin/nologin\n",
    )
    write(
        "/etc/group",
        "root:x:0:\n"
        "wheel:x:10:\n"
        "video:x:27:\n"
        "input:x:97:\n"
        "ddm:x:990:\n",
    )
    write(
        "/etc/shadow",
        "root:!:0:0:99999:7:::\n"
        "ddm:!:0:0:99999:7:::\n",
        0o600,
    )
    write(
        "/etc/gshadow",
        "root:*::\nwheel:*::\nddm:!::\n",
        0o600,
    )
    write("/etc/profile", "export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin\nexport TERM=${TERM:-xterm-256color}\nexport COLORTERM=truecolor\nexport LANG=zh_CN.UTF-8\nexport PS1='\\u@\\h:\\w\\$ '\n")
    write("/etc/os-release", 'NAME="SpaceOS"\nVERSION="9"\nID=spaceos9\nID_LIKE=alpine\nVERSION_ID="9"\nPRETTY_NAME="SpaceOS 9"\nHOME_URL="https://github.com/ViudiraTech/SpaceOS-9"\n')
    write("/etc/fstab", "/dev/nvme0n1 / ext4 defaults 0 1\nproc /proc proc defaults 0 0\nsysfs /sys sysfs defaults 0 0\ndevtmpfs /dev devtmpfs defaults 0 0\ntmpfs /tmp tmpfs defaults 0 0\n")
    write("/etc/motd", "SpaceOS 9 desktop (Alpine userland, ext4 + jbd2)\nGraphical login is provided by DDM; root graphical login is disabled.\n")
    write("/etc/locale.conf", "LANG=zh_CN.UTF-8\n")
    write("/etc/TZ", "UTC0\n")
    write("/etc/timezone", "UTC\n")
    localtime = STAGE / "etc" / "localtime"
    if localtime.exists() or localtime.is_symlink():
        localtime.unlink()
    os.symlink("/usr/share/zoneinfo/UTC", localtime)

    ddm_dir = ROOT / "user" / "desktop" / "assets" / "ddm"
    if (ddm_dir / "ddm.conf").is_file():
        install_binary(ddm_dir / "ddm.conf", "/etc/ddm/ddm.conf", 0o644)
    if (ddm_dir / "doas.conf").is_file():
        install_binary(ddm_dir / "doas.conf", "/etc/doas.conf", 0o640)
    xsessions = ROOT / "user" / "desktop" / "assets" / "xsessions"
    if xsessions.is_dir():
        for path in sorted(xsessions.iterdir()):
            if path.is_file():
                install_binary(path, f"/usr/share/xsessions/{path.name}", 0o644)

    # Glamor pulls in Mesa and LLVM. Keep them on disk for a later GL stage, but
    # never load them: this is the Pixman path chosen for the first release.
    write("/etc/X11/xorg.conf.d/10-spaceos.conf", """Section "ServerFlags"
    Option "AutoAddGPU" "false"
    Option "DontVTSwitch" "true"
    Option "BlankTime" "0"
    Option "StandbyTime" "0"
    Option "SuspendTime" "0"
    Option "OffTime" "0"
EndSection

Section "Module"
    Disable "glamoregl"
    Disable "glx"
    Disable "dri"
    Disable "dri2"
    Disable "dri3"
EndSection

Section "Monitor"
    Identifier "Virtual-1"
    Modeline "1920x1080" 173.00 1920 2048 2248 2576 1080 1083 1088 1120 -hsync +vsync
    Modeline "1600x900"  118.25 1600 1696 1856 2112  900  903  908  934 -hsync +vsync
    Modeline "1440x900"  106.50 1440 1528 1672 1904  900  903  909  934 -hsync +vsync
    Modeline "1280x800"   83.50 1280 1352 1480 1680  800  803  809  831 -hsync +vsync
    Modeline "1024x768"   63.50 1024 1072 1176 1328  768  771  775  798 -hsync +vsync
    Option "PreferredMode" "1280x800"
EndSection

Section "Device"
    Identifier "virtio"
    Driver "modesetting"
    Option "AccelMethod" "none"
    Option "PageFlip" "false"
    Option "ShadowFB" "true"
    Option "SWcursor" "true"
    Option "Monitor-Virtual-1" "Virtual-1"
EndSection

Section "Screen"
    Identifier "DefaultScreen"
    Device "virtio"
    Monitor "Virtual-1"
    DefaultDepth 24
    SubSection "Display"
        Depth 24
        Virtual 1920 1200
    EndSubSection
EndSection

Section "Extensions"
    Option "GLX" "Disable"
    Option "XVideo" "Disable"
EndSection
""")
    write("/etc/X11/xorg.conf.d/20-input.conf", """Section "InputClass"
    Identifier "keyboard"
    MatchIsKeyboard "on"
    Driver "libinput"
    Option "XkbLayout" "us"
EndSection

Section "InputClass"
    Identifier "pointer"
    MatchIsPointer "on"
    Driver "libinput"
EndSection

Section "InputClass"
    Identifier "tablet"
    MatchIsTablet "on"
    Driver "libinput"
EndSection
""")

    services = pathlib.Path(os.environ.get("SPACEOS_SERVICES", ROOT / "user" / "desktop" / "services"))
    if services.is_dir():
        for path in sorted(services.iterdir()):
            write(f"/etc/systemg/{path.name}", path.read_text())

    for name in ("systemg",):
        source = USERLAND / name
        if source.exists():
            install_binary(source, f"/sbin/{name}")
    for name in ("hello", "systemctl", "systemg-analyze"):
        source = USERLAND / name
        if source.exists():
            install_binary(source, f"/bin/{name}")
            for p in (f"/usr/bin/{name}", f"/sbin/{name}"):
                dest = STAGE / p.lstrip("/")
                if dest.is_symlink() or dest.exists():
                    dest.unlink()
                dest.symlink_to(f"/bin/{name}")

    for p in ("/bin/systemd-analyze", "/usr/bin/systemd-analyze"):
        dest = STAGE / p.lstrip("/")
        if dest.is_symlink() or dest.exists():
            dest.unlink()
        dest.symlink_to("/bin/systemg-analyze")

    spui = ROOT / "target" / "x86_64-unknown-linux-musl" / os.environ.get("SPUI_PROFILE", "release")
    if spui.is_dir():
        for path in sorted(spui.iterdir()):
            if path.is_file() and os.access(path, os.X_OK) and path.name in ("spui", "uxconfd", "uxconf-query"):
                install_binary(path, f"/usr/bin/{path.name}")
    assets = ROOT / "user" / "desktop" / "assets"
    if assets.is_dir():
        for path in assets.rglob("*"):
            if path.is_file():
                install_binary(path, f"/usr/share/spui/{path.relative_to(assets)}", 0o644)
    session_script = STAGE / "usr" / "share" / "spui" / "session.sh"
    if session_script.is_file():
        os.chmod(session_script, 0o755)

    install_spui_assets()
    generate_misc_font_index()

    # Autotools-style module search needs this directory even when empty.
    (STAGE / "usr" / "share" / "fonts").mkdir(parents=True, exist_ok=True)
    # The Xorg wrapper needs a setuid helper; root starts the server directly.
    wrapper = STAGE / "usr" / "bin" / "Xorg"
    if wrapper.exists() or wrapper.is_symlink():
        wrapper.unlink()
    os.symlink("/usr/libexec/Xorg", wrapper)


def make_image(size_mib: int) -> None:
    if IMAGE.exists():
        IMAGE.unlink()
    with open(IMAGE, "wb") as image:
        image.truncate(size_mib * 1024 * 1024)
    # fakeroot gives every file owner 0:0 without running as root.
    subprocess.run(
        [
            "fakeroot", "/usr/sbin/mke2fs", "-q", "-F", "-t", "ext4", "-b", "4096",
            "-O", EXT4_FEATURES, "-E", "lazy_itable_init=0,lazy_journal_init=0",
            "-J", "size=64", "-L", "SPACEROOT", "-d", str(STAGE), str(IMAGE),
        ],
        check=True,
    )
    subprocess.run(["/usr/sbin/e2fsck", "-fn", str(IMAGE)], check=True, stdout=subprocess.DEVNULL)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--size-mib", type=int, default=2048)
    parser.add_argument("--reuse-stage", action="store_true",
                        help="Skip package extraction and only refresh SpaceOS files")
    parser.add_argument("--image", type=pathlib.Path,
                        help="Write the ext4 image here instead of build/rootfs/ext4.img")
    arguments = parser.parse_args()
    if arguments.image:
        global IMAGE
        IMAGE = arguments.image.resolve()
        IMAGE.parent.mkdir(parents=True, exist_ok=True)

    BUILD.mkdir(parents=True, exist_ok=True)
    if not arguments.reuse_stage or not STAGE.exists():
        packages, providers = load_indexes()
        selected = resolve(packages, providers)
        total = sum(int(record.get("I", 0)) for record in selected.values())
        log(f"{len(selected)} packages, {total / 1048576:.0f} MiB installed")
        if STAGE.exists():
            shutil.rmtree(STAGE)
        STAGE.mkdir(parents=True)
        for name in ("musl",):
            extract(download(selected[name]))
        for name, record in sorted(selected.items()):
            if name != "musl":
                extract(download(record))
        (BUILD / "packages.txt").write_text(
            "".join(f"{name} {selected[name]['V']}\n" for name in sorted(selected))
        )
        busybox_links()
    configure()
    make_image(arguments.size_mib)
    log(f"image ready: {IMAGE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
