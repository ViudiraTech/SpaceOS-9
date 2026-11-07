#!/usr/bin/env python3
#
#       tools/desktop_vm.py
#       Boot and drive the desktop image in QEMU for development and tests
#
#       2026/10/30 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Control a headless desktop VM through its serial console and QMP.

  start        boot the VM in the background (KVM when available)
  stop         power the VM off the hard way
  run CMD      run a shell command on the serial console, print its output
  shot FILE    save the current scanout as a PNG
  key KEYS     press QKeyCode names at once, e.g. "ctrl alt t"
  move X Y     move the absolute pointer to a screen coordinate
  click [BTN]  press and release left, right or middle
  drag X Y X2 Y2  drag with the left button held
  log [N]      print the last N lines of the serial log
"""

import argparse
import base64
import json
import os
import pathlib
import re
import socket
import struct
import subprocess
import sys
import time
import uuid
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
RUN = pathlib.Path(os.environ.get("DESKTOP_VM_RUN", ROOT / "build" / "desktop-vm"))
SERIAL = RUN / "serial.sock"
QMP = RUN / "qmp.sock"
LOG = RUN / "serial.log"
PID = RUN / "qemu.pid"
IMAGE = pathlib.Path(os.environ.get("DESKTOP_VM_IMAGE", ROOT / "build" / "rootfs" / "ext4.img"))
FIRMWARE = "/usr/share/OVMF/OVMF_CODE_4M.fd"
SCREEN = (1280, 800)


def iso_path() -> pathlib.Path:
    for profile in ("debug", "release"):
        candidate = ROOT / "build" / "x86_64" / profile / "SpaceKernel-x86_64.iso"
        if candidate.exists():
            return candidate
    raise SystemExit("no ISO; run `make desktop-iso` first")


def start(arguments: argparse.Namespace) -> int:
    RUN.mkdir(parents=True, exist_ok=True)
    for path in (SERIAL, QMP):
        if path.exists():
            path.unlink()
    LOG.write_text("")
    iso = pathlib.Path(arguments.iso) if arguments.iso else iso_path()
    accel = ["-enable-kvm", "-cpu", "host"] if os.access("/dev/kvm", os.W_OK) else ["-cpu", "max"]
    command = [
        "qemu-system-x86_64", *accel,
        "-machine", "q35",
        "-m", str(arguments.memory),
        "-smp", str(arguments.cpus),
        "-display", arguments.display,
        "-no-reboot",
        "-chardev", f"socket,id=ser0,path={SERIAL},server=on,wait=off,logfile={LOG}",
        "-serial", "chardev:ser0",
        "-qmp", f"unix:{QMP},server=on,wait=off",
        "-monitor", "none",
        *(["-s"] if arguments.gdb else []),
        "-device", "virtio-gpu-pci,id=desktop-gpu,xres=1280,yres=800",
        "-device", "virtio-tablet-pci",
        "-device", "virtio-keyboard-pci",
        *(["-nic", arguments.nic] if arguments.nic != "none" else []),
        "-drive", f"file={IMAGE},if=none,id=root,format=raw",
        "-device", "nvme,serial=SPROOT,drive=root",
        "-drive", f"if=pflash,format=raw,readonly=on,file={FIRMWARE}",
        "-cdrom", str(iso),
        "-daemonize", "-pidfile", str(PID),
    ]
    subprocess.run(command, check=True)
    print(f"VM started, log: {LOG}")
    return 0


def stop(_arguments: argparse.Namespace) -> int:
    if PID.exists():
        try:
            os.kill(int(PID.read_text()), 15)
        except (ProcessLookupError, ValueError):
            pass
        for _ in range(50):
            if not PID.exists():
                break
            try:
                os.kill(int(PID.read_text()), 0)
            except (ProcessLookupError, ValueError, FileNotFoundError):
                break
            time.sleep(0.1)
        PID.unlink(missing_ok=True)
    return 0


def serial_command(command: str, timeout: float) -> tuple[str, int]:
    marker = uuid.uuid4().hex[:12]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(str(SERIAL))
    sock.settimeout(0.25)
    # Long lines exceed what the shell's line editor accepts over a raw serial
    # link, so anything sizable travels as a script written in short chunks.
    if len(command) > 120:
        script = f"/tmp/.vm-{marker}"
        encoded = base64.b64encode(command.encode()).decode()
        for start in range(0, len(encoded), 96):
            part = encoded[start : start + 96]
            sock.sendall(f"printf %s '{part}' >> {script}.b64\n".encode())
            time.sleep(0.03)
        line = f"base64 -d {script}.b64 > {script}; rm {script}.b64; sh {script} 2>&1; echo {marker}=$?\n"
    else:
        line = f"({command}) 2>&1; echo {marker}=$?\n"
    sock.sendall(line.encode())
    output = b""
    deadline = time.monotonic() + timeout
    pattern = re.compile(rb"\n" + marker.encode() + rb"=(\d+)")
    while time.monotonic() < deadline:
        try:
            chunk = sock.recv(65536)
        except TimeoutError:
            chunk = b""
        if chunk:
            output += chunk
        # The echoed command contains "marker=$?", the result contains digits.
        match = pattern.search(output)
        if match:
            text = output[: match.start()].decode("utf-8", "replace")
            first = text.rfind(marker)
            first = text.find("\n", first) if first >= 0 else -1
            return text[first + 1 :] if first >= 0 else text, int(match.group(1))
    return output.decode("utf-8", "replace"), 124


def run(arguments: argparse.Namespace) -> int:
    text, code = serial_command(" ".join(arguments.command), arguments.timeout)
    sys.stdout.write(text.replace("\r\n", "\n"))
    return code


class Qmp:
    def __init__(self) -> None:
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(str(QMP))
        self.file = self.sock.makefile("rwb")
        json.loads(self.file.readline())
        self.call("qmp_capabilities")

    def call(self, command: str, **parameters) -> dict:
        self.file.write(json.dumps({"execute": command, "arguments": parameters}).encode() + b"\n")
        self.file.flush()
        while True:
            reply = json.loads(self.file.readline())
            if "return" in reply or "error" in reply:
                return reply


def ppm_to_png(source: pathlib.Path, destination: pathlib.Path) -> None:
    data = source.read_bytes()
    match = re.match(rb"P6\s+(\d+)\s+(\d+)\s+(\d+)\s", data)
    if not match:
        raise SystemExit("not a PPM file")
    width, height = int(match.group(1)), int(match.group(2))
    pixels = data[match.end() :]
    stride = width * 3
    raw = b"".join(b"\0" + pixels[y * stride : (y + 1) * stride] for y in range(height))

    def chunk(kind: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + kind
            + payload
            + struct.pack(">I", zlib.crc32(kind + payload))
        )

    destination.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 6))
        + chunk(b"IEND", b"")
    )


def shot(arguments: argparse.Namespace) -> int:
    destination = pathlib.Path(arguments.file).resolve()
    temporary = RUN / "screen.ppm"
    reply = Qmp().call("screendump", filename=str(temporary), device="desktop-gpu")
    if "error" in reply:
        print(reply["error"], file=sys.stderr)
        return 1
    for _ in range(50):
        if temporary.exists() and temporary.stat().st_size > 0:
            break
        time.sleep(0.05)
    ppm_to_png(temporary, destination)
    temporary.unlink()
    print(destination)
    return 0


def key(arguments: argparse.Namespace) -> int:
    keys = [{"type": "qcode", "data": name} for name in arguments.keys]
    reply = Qmp().call("send-key", keys=keys, **{"hold-time": arguments.hold})
    return 1 if "error" in reply else 0


def move(arguments: argparse.Namespace) -> int:
    events = [
        {"type": "abs", "data": {"axis": "x", "value": arguments.x * 32767 // (SCREEN[0] - 1)}},
        {"type": "abs", "data": {"axis": "y", "value": arguments.y * 32767 // (SCREEN[1] - 1)}},
    ]
    reply = Qmp().call("input-send-event", events=events)
    return 1 if "error" in reply else 0


def click(arguments: argparse.Namespace) -> int:
    qmp = Qmp()
    for down in (True, False):
        event = {"type": "btn", "data": {"down": down, "button": arguments.button}}
        reply = qmp.call("input-send-event", events=[event])
        if "error" in reply:
            print(reply["error"], file=sys.stderr)
            return 1
        time.sleep(0.05)
    return 0


def drag(arguments: argparse.Namespace) -> int:
    """Press at (x, y), move to (x2, y2) in steps, release; print the elapsed time."""
    qmp = Qmp()

    def position(x: int, y: int) -> list:
        return [
            {"type": "abs", "data": {"axis": "x", "value": x * 32767 // (SCREEN[0] - 1)}},
            {"type": "abs", "data": {"axis": "y", "value": y * 32767 // (SCREEN[1] - 1)}},
        ]

    def button(down: bool) -> list:
        return [{"type": "btn", "data": {"down": down, "button": "left"}}]

    qmp.call("input-send-event", events=position(arguments.x, arguments.y))
    time.sleep(0.2)
    qmp.call("input-send-event", events=button(True))
    time.sleep(0.2)
    started = time.monotonic()
    for step in range(1, arguments.steps + 1):
        x = arguments.x + (arguments.x2 - arguments.x) * step // arguments.steps
        y = arguments.y + (arguments.y2 - arguments.y) * step // arguments.steps
        qmp.call("input-send-event", events=position(x, y))
        time.sleep(arguments.delay)
    elapsed = time.monotonic() - started
    time.sleep(0.2)
    qmp.call("input-send-event", events=button(False))
    print(f"drag: {arguments.steps} moves sent in {elapsed:.2f}s")
    return 0


def text(arguments: argparse.Namespace) -> int:
    qmp = Qmp()
    names = {" ": "spc", "\n": "ret", "-": "minus", ".": "dot", "/": "slash", "=": "equal"}
    for character in " ".join(arguments.text):
        if character.isupper():
            keys = ["shift", character.lower()]
        else:
            keys = [names.get(character, character)]
        qmp.call(
            "send-key",
            keys=[{"type": "qcode", "data": name} for name in keys],
            **{"hold-time": 30},
        )
        time.sleep(0.04)
    return 0


def show_log(arguments: argparse.Namespace) -> int:
    lines = LOG.read_text(errors="replace").splitlines()
    print("\n".join(lines[-arguments.lines :]))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    start_parser = commands.add_parser("start")
    start_parser.add_argument("--memory", type=int, default=2048)
    start_parser.add_argument("--cpus", type=int, default=4)
    start_parser.add_argument("--iso")
    start_parser.add_argument("--display", default="none",
                              help="QEMU display backend (none, sdl, gtk)")
    start_parser.add_argument("--nic", default="user,model=e1000e",
                              help="QEMU -nic specification (default: user,model=e1000e; 'none' disables)")
    start_parser.add_argument("--gdb", action="store_true",
                              help="Expose the QEMU GDB stub on tcp::1234")
    start_parser.set_defaults(handler=start)
    commands.add_parser("stop").set_defaults(handler=stop)
    run_parser = commands.add_parser("run")
    run_parser.add_argument("command", nargs="+")
    run_parser.add_argument("--timeout", type=float, default=30)
    run_parser.set_defaults(handler=run)
    shot_parser = commands.add_parser("shot")
    shot_parser.add_argument("file")
    shot_parser.set_defaults(handler=shot)
    key_parser = commands.add_parser("key")
    key_parser.add_argument("keys", nargs="+")
    key_parser.add_argument("--hold", type=int, default=60)
    key_parser.set_defaults(handler=key)
    move_parser = commands.add_parser("move")
    move_parser.add_argument("x", type=int)
    move_parser.add_argument("y", type=int)
    move_parser.set_defaults(handler=move)
    click_parser = commands.add_parser("click")
    click_parser.add_argument("button", nargs="?", default="left")
    click_parser.set_defaults(handler=click)
    drag_parser = commands.add_parser("drag")
    for name in ("x", "y", "x2", "y2"):
        drag_parser.add_argument(name, type=int)
    drag_parser.add_argument("--steps", type=int, default=30)
    drag_parser.add_argument("--delay", type=float, default=0.02)
    drag_parser.set_defaults(handler=drag)
    text_parser = commands.add_parser("text")
    text_parser.add_argument("text", nargs="+")
    text_parser.set_defaults(handler=text)
    log_parser = commands.add_parser("log")
    log_parser.add_argument("lines", nargs="?", type=int, default=40)
    log_parser.set_defaults(handler=show_log)
    arguments = parser.parse_args()
    return arguments.handler(arguments)


if __name__ == "__main__":
    sys.exit(main())
