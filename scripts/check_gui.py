#!/usr/bin/env python3
"""Real Linux/X11 GUI integration test (developer tooling, not a runtime dependency).

Uses only Python's standard library and the system X11/XTest libraries. It starts
an isolated Xvfb, copies the executable outside the checkout, clears its PATH,
and drives the actual native window. Independent ExifTool is an oracle used by
this test process only; the application cannot find it. Run prepare_gui_test.sh
once to obtain the local Xvfb. Screenshots and logs go to target/test-artifacts.
"""

import argparse
import ctypes as C
import ctypes.util
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib

from gui_visual import assert_map_canvas, assert_selected_tab, compact_tab_points, photo_geometry


ROOT = Path(__file__).resolve().parents[1]


def png(path, width, height, rgb):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    rows = b"".join(b"\0" + rgb[y * width * 3:(y + 1) * width * 3] for y in range(height))
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def preserved_read(path, operation):
    before = path.stat()
    try:
        return operation()
    finally:
        after = path.stat()
        if (before.st_ino, before.st_size, before.st_mtime_ns) != (after.st_ino, after.st_size, after.st_mtime_ns):
            raise RuntimeError(f"File changed while reading: {path}")
        os.utime(path, ns=(before.st_atime_ns, after.st_mtime_ns))


def snapshot(path):
    info = path.stat()
    birth = getattr(info, "st_birthtime_ns", None)
    libc = C.CDLL(None, use_errno=True) if sys.platform == "linux" and birth is None else None
    if libc is not None and hasattr(libc, "statx"):
        statx = libc.statx
        statx.argtypes = [C.c_int, C.c_char_p, C.c_int, C.c_uint, C.c_void_p]
        statx.restype = C.c_int
        data = C.create_string_buffer(256)
        if statx(-100, os.fsencode(path), 0x100, 0xFFF, data) == 0 and struct.unpack_from("I", data, 0)[0] & 0x800:
            seconds, nanos = struct.unpack_from("qI", data, 80)
            birth = seconds * 1_000_000_000 + nanos
    return {"accessed_ns": info.st_atime_ns, "modified_ns": info.st_mtime_ns,
            "created_ns": birth, "inode": info.st_ino}


def print_failure_logs(artifacts):
    """Report bounded child-process diagnostics, without dumping environment."""
    for name in ("gui-stderr.log", "gui-xvfb.log"):
        path = artifacts / name
        try:
            with path.open("rb") as log:
                log.seek(0, os.SEEK_END)
                length = log.tell()
                log.seek(max(0, length - 8192))
                text = log.read(8192).decode("utf-8", errors="replace")
            lines = text.splitlines()
            truncated = length > 8192 or len(lines) > 60
            tail = "\n".join(lines[-60:]).strip() or "(empty)"
            label = f"--- {name}: last 8 KiB / 60 lines"
            print(label + (" (truncated)" if truncated else "") + " ---", file=sys.stderr)
            print(tail, file=sys.stderr, flush=True)
        except OSError as error:
            print(f"--- {name}: unable to read diagnostic log: {error} ---", file=sys.stderr, flush=True)


class Oracle:
    def __init__(self):
        perl = shutil.which("perl")
        exiftool = Path(os.environ.get("PHOTOPINFIX_TEST_EXIFTOOL", ROOT / "tools/exiftool/exiftool")).resolve()
        if not perl or not exiftool.is_file():
            raise RuntimeError("Full GUI verification needs developer ExifTool and Perl; set PHOTOPINFIX_TEST_EXIFTOOL or provide tools/exiftool/exiftool. The app runtime does not need them.")
        self.command = [perl, str(exiftool), "-config", str(ROOT / "assets/photopinfix-exiftool.config")]

    def run(self, *args):
        result = subprocess.run(self.command + list(args), capture_output=True, check=True, timeout=30)
        return result.stdout

    def read(self, path):
        return preserved_read(path, lambda: json.loads(self.run(
            "-j", "-n", "-GPSLatitude", "-GPSLongitude", "-XMP-PhotoPinFix:Mode", "-XMP-PhotoPinFix:Method", str(path)))[0])


def fixtures(directory, oracle, documentation=False):
    rows = [
        ("001_已有定位.heic", "10:00:00", (-33.6, -70.5)),
        ("002_森林溪流.jpg" if documentation else "002_手动编辑.png", "10:00:30", None),
        ("003_自动补全.png", "10:00:45", None),
        ("004_已有定位.png", "10:01:00", (-33.4, -70.1)),
        ("005_另一次拍摄.png", "12:00:00", None),
    ]
    paths = []
    original = {}
    times = {}
    for index, (name, clock, coordinates) in enumerate(rows):
        path = directory / name
        if path.suffix == ".heic":
            source = ROOT / "tests/fixtures/heic/gradient.heic"
            preserved_read(source, lambda: shutil.copyfile(source, path))
        elif path.suffix == ".jpg":
            source = ROOT / "tests/fixtures/gui/landscape.jpg"
            preserved_read(source, lambda: shutil.copyfile(source, path))
        else:
            pixels = bytes(value for y in range(96) for x in range(144)
                           for value in ((x * 255 // 143), (y * 255 // 95), (index * 45)))
            png(path, 144, 96, pixels)
        args = ["-overwrite_original", f"-DateTimeOriginal=2026:11:22 {clock}",
                f"-CreateDate=2026:11:22 {clock}", "-GPS:All=", "-XMP-exif:GPSLatitude=", "-XMP-exif:GPSLongitude=",
                "-XMP-PhotoPinFix:Mode=", "-XMP-PhotoPinFix:Method="]
        if coordinates:
            lat, lon = coordinates
            args += [f"-GPSLatitude={abs(lat)}", f"-GPSLatitudeRef={'S' if lat < 0 else 'N'}",
                     f"-GPSLongitude={abs(lon)}", f"-GPSLongitudeRef={'W' if lon < 0 else 'E'}"]
        oracle.run(*args, str(path))
        original[path] = path.read_bytes()
        # Exact old, distinct nanosecond values; no tolerance hides a regression.
        os.utime(path, ns=(1_600_000_000_123_456_700 + index * 100, 1_600_001_000_765_432_100 + index * 100))
        times[path] = snapshot(path)
        paths.append(path)
    return paths, original, times


class WindowAttributes(C.Structure):
    _fields_ = [(name, kind) for name, kind in (
        ("x", C.c_int), ("y", C.c_int), ("width", C.c_int), ("height", C.c_int),
        ("border_width", C.c_int), ("depth", C.c_int), ("visual", C.c_void_p), ("root", C.c_ulong),
        ("class_", C.c_int), ("bit_gravity", C.c_int), ("win_gravity", C.c_int), ("backing_store", C.c_int),
        ("backing_planes", C.c_ulong), ("backing_pixel", C.c_ulong), ("save_under", C.c_int),
        ("colormap", C.c_ulong), ("map_installed", C.c_int), ("map_state", C.c_int),
        ("all_event_masks", C.c_long), ("your_event_mask", C.c_long), ("do_not_propagate_mask", C.c_long),
        ("override_redirect", C.c_int), ("screen", C.c_void_p))]


class XImage(C.Structure):
    _fields_ = [("width", C.c_int), ("height", C.c_int), ("xoffset", C.c_int), ("format", C.c_int),
                ("data", C.c_void_p), ("byte_order", C.c_int), ("bitmap_unit", C.c_int),
                ("bitmap_bit_order", C.c_int), ("bitmap_pad", C.c_int), ("depth", C.c_int),
                ("bytes_per_line", C.c_int), ("bits_per_pixel", C.c_int),
                ("red_mask", C.c_ulong), ("green_mask", C.c_ulong), ("blue_mask", C.c_ulong)]


class X11:
    def __init__(self, display_name):
        self.lib = C.CDLL(ctypes.util.find_library("X11"))
        self.xtest = C.CDLL(ctypes.util.find_library("Xtst"))
        signatures = {
            "XOpenDisplay": ([C.c_char_p], C.c_void_p),
            "XDefaultRootWindow": ([C.c_void_p], C.c_ulong),
            "XConnectionNumber": ([C.c_void_p], C.c_int),
            "XPending": ([C.c_void_p], C.c_int),
            "XNextEvent": ([C.c_void_p, C.c_void_p], C.c_int),
            "XSelectInput": ([C.c_void_p, C.c_ulong, C.c_long], C.c_int),
            "XQueryTree": ([C.c_void_p, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_ulong), C.POINTER(C.POINTER(C.c_ulong)), C.POINTER(C.c_uint)], C.c_int),
            "XFetchName": ([C.c_void_p, C.c_ulong, C.POINTER(C.c_char_p)], C.c_int),
            "XFree": ([C.c_void_p], C.c_int),
            "XGetWindowAttributes": ([C.c_void_p, C.c_ulong, C.POINTER(WindowAttributes)], C.c_int),
            "XSetInputFocus": ([C.c_void_p, C.c_ulong, C.c_int, C.c_ulong], C.c_int),
            "XStringToKeysym": ([C.c_char_p], C.c_ulong),
            "XKeysymToKeycode": ([C.c_void_p, C.c_ulong], C.c_ubyte),
            "XFlush": ([C.c_void_p], C.c_int),
            "XResizeWindow": ([C.c_void_p, C.c_ulong, C.c_uint, C.c_uint], C.c_int),
            "XTranslateCoordinates": ([C.c_void_p, C.c_ulong, C.c_ulong, C.c_int, C.c_int, C.POINTER(C.c_int), C.POINTER(C.c_int), C.POINTER(C.c_ulong)], C.c_int),
            "XGetImage": ([C.c_void_p, C.c_ulong, C.c_int, C.c_int, C.c_uint, C.c_uint, C.c_ulong, C.c_int], C.POINTER(XImage)),
            "XDestroyImage": ([C.POINTER(XImage)], C.c_int),
            "XCloseDisplay": ([C.c_void_p], C.c_int),
        }
        for name, (args, result) in signatures.items():
            fn = getattr(self.lib, name)
            fn.argtypes, fn.restype = args, result
        self.xtest.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        self.xtest.XTestFakeKeyEvent.restype = C.c_int
        self.xtest.XTestFakeMotionEvent.argtypes = [C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong]
        self.xtest.XTestFakeMotionEvent.restype = C.c_int
        self.xtest.XTestFakeButtonEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        self.xtest.XTestFakeButtonEvent.restype = C.c_int
        self.display = self.lib.XOpenDisplay(display_name.encode())
        if not self.display:
            raise RuntimeError(f"XOpenDisplay failed: {display_name}")
        self.root = self.lib.XDefaultRootWindow(self.display)
        self.fd = self.lib.XConnectionNumber(self.display)
        self.lib.XSelectInput(self.display, self.root, (1 << 19) | (1 << 17))
        self.window = None
        self.process = None

    def events(self, timeout=0.05):
        event = (C.c_long * 24)()
        while self.lib.XPending(self.display):
            self.lib.XNextEvent(self.display, C.byref(event))
        select.select([self.fd], [], [], timeout)
        if self.process and self.process.poll() is not None:
            raise RuntimeError(f"GUI exited early with status {self.process.returncode}; inspect gui stderr log")

    def wait(self, predicate, description, timeout=40):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            self.events()
        raise RuntimeError(f"Timed out: {description}")

    def find_window(self):
        root, parent, children, count = C.c_ulong(), C.c_ulong(), C.POINTER(C.c_ulong)(), C.c_uint()
        if not self.lib.XQueryTree(self.display, self.root, C.byref(root), C.byref(parent), C.byref(children), C.byref(count)):
            return None
        try:
            for index in range(count.value):
                window = children[index]
                title = C.c_char_p()
                if self.lib.XFetchName(self.display, window, C.byref(title)) and title.value:
                    try:
                        if b"PhotoPinFix" in title.value and self.attributes(window).map_state == 2:
                            self.window = window
                            self.lib.XSelectInput(self.display, window, (1 << 17) | (1 << 15))
                            return window
                    finally:
                        self.lib.XFree(title)
        finally:
            if children:
                self.lib.XFree(children)
        return None

    def attributes(self, window=None):
        attrs = WindowAttributes()
        if not self.lib.XGetWindowAttributes(self.display, window or self.window, C.byref(attrs)):
            raise RuntimeError("Cannot inspect GUI window")
        return attrs

    def key(self, name, modifiers=()):
        self.lib.XSetInputFocus(self.display, self.window, 1, 0)

        def event(key, pressed):
            symbol = self.lib.XStringToKeysym(key.encode())
            code = self.lib.XKeysymToKeycode(self.display, symbol)
            if not code:
                raise RuntimeError(f"No X11 keycode for {key}")
            if not self.xtest.XTestFakeKeyEvent(self.display, code, pressed, 0):
                raise RuntimeError("XTest key injection failed")

        for modifier in modifiers:
            event(modifier, 1)
        event(name, 1)
        event(name, 0)
        for modifier in reversed(modifiers):
            event(modifier, 0)
        self.lib.XFlush(self.display)
        # Drain the X event stream between keystrokes so text input and focus
        # transitions land in separate egui frames, as on a physical keyboard.
        self.events(0.06)

    def text(self, value):
        for char in value:
            self.key({"-": "minus", ".": "period"}.get(char, char))

    def click(self, x, y):
        self.lib.XSetInputFocus(self.display, self.window, 1, 0)
        x, y = round(x), round(y)
        root_x, root_y, child = C.c_int(), C.c_int(), C.c_ulong()
        if not self.lib.XTranslateCoordinates(self.display, self.window, self.root, x, y,
                                             C.byref(root_x), C.byref(root_y), C.byref(child)):
            raise RuntimeError("Could not translate window coordinates")
        self.xtest.XTestFakeMotionEvent(self.display, -1, root_x.value, root_y.value, 0)
        self.xtest.XTestFakeButtonEvent(self.display, 1, 1, 0)
        self.xtest.XTestFakeButtonEvent(self.display, 1, 0, 0)
        self.lib.XFlush(self.display)
        self.events(0.06)

    def frame(self):
        attrs = self.attributes()
        ptr = self.lib.XGetImage(self.display, self.window, 0, 0, attrs.width, attrs.height, C.c_ulong(-1).value, 2)
        if not ptr:
            raise RuntimeError("XGetImage failed")
        try:
            image = ptr.contents
            if image.bits_per_pixel != 32:
                raise RuntimeError(f"Expected 32-bit Xvfb image, got {image.bits_per_pixel}")
            data = C.string_at(image.data, image.bytes_per_line * image.height)
            channels = []
            for mask in (image.red_mask, image.green_mask, image.blue_mask):
                offset = (mask.bit_length() - 8) // 8
                channels.append(offset if image.byte_order == 0 else 3 - offset)
            pixels = bytearray(image.width * image.height * 3)
            for y in range(image.height):
                row = data[y * image.bytes_per_line:y * image.bytes_per_line + image.width * 4]
                start = y * image.width * 3
                for channel, offset in enumerate(channels):
                    pixels[start + channel:start + image.width * 3:3] = row[offset::4]
            return image.width, image.height, bytes(pixels)
        finally:
            self.lib.XDestroyImage(ptr)

    def settled(self, path=None, timeout=12):
        previous, stable_since, latest = None, time.monotonic(), None

        def stable():
            nonlocal previous, stable_since, latest
            latest = self.frame()
            digest = hashlib.sha256(latest[2]).digest()
            if digest != previous:
                previous, stable_since = digest, time.monotonic()
                return False
            return time.monotonic() - stable_since > 0.35

        self.wait(stable, "window renders a stable frame", timeout)
        if len(set(latest[2][::3])) < 12:
            raise RuntimeError("Window appears blank")
        if path:
            png(path, *latest)
        return latest

    def close(self):
        self.lib.XCloseDisplay(self.display)


def verify(oracle, paths, original, expected_times):
    for path in paths:
        observed = snapshot(path)
        if observed != expected_times[path]:
            raise AssertionError(f"File times/identity changed for {path.name}: {expected_times[path]} -> {observed}")
    for index, path in enumerate(paths):
        data = oracle.read(path)
        current = preserved_read(path, path.read_bytes)
        if index in (1, 2):
            # Writing the manual photo does not implicitly recompute the other
            # reviewed proposals. At 10:00:45, the original 10:00/10:01 anchors
            # still produce -70.2; -70.175 would require an explicit recompute.
            expected = (-33.5, -70.25, "manual", "coordinate-input") if index == 1 else (-33.45, -70.2, "auto", "time-interpolation")
            for key, value in zip(("GPSLatitude", "GPSLongitude", "Mode", "Method"), expected):
                actual = data.get(key)
                if isinstance(value, float):
                    assert actual is not None and abs(actual - value) < 1e-7, (path.name, key, actual, value)
                else:
                    assert actual == value, (path.name, key, actual, value)
            backup = path.with_name(path.name + ".photopinfix-original")
            assert backup.read_bytes() == original[path], f"Original backup differs: {path.name}"
            assert current != original[path], f"GPS was not written: {path.name}"
        else:
            assert current == original[path], f"Untargeted image changed: {path.name}"
        assert snapshot(path) == expected_times[path], f"Test oracle failed to preserve access time: {path.name}"


def run(args):
    binary = Path(args.binary).resolve()
    if not binary.is_file():
        raise RuntimeError(f"Executable does not exist: {binary}")
    artifacts = Path(args.artifacts).resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    screenshots = Path(args.screenshots).resolve() if args.screenshots else None
    xvfb_override = os.environ.get("PHOTOPINFIX_TEST_XVFB")
    xvfb = Path(xvfb_override).resolve() if xvfb_override else ROOT / "tools/gui-test/root/usr/bin/Xvfb"
    if not xvfb.exists() and not xvfb_override:
        xvfb = Path(shutil.which("Xvfb") or "/missing-Xvfb")
    if not xvfb.exists():
        raise RuntimeError("Run scripts/prepare_gui_test.sh first (local Xvfb is missing)")
    oracle = Oracle()
    xserver = application = x = None
    # Documentation frames show only this disposable, generic directory. Do
    # not expose a developer's home, checkout or personal photograph locations.
    with tempfile.TemporaryDirectory(prefix="photopinfix-demo-" if screenshots else "photopinfix-gui-", dir="/tmp" if screenshots else None) as temporary:
        temp = Path(temporary)
        # Some agent sandboxes deny bind(2) even for a process's own Unix
        # sockets. Detect that before Xvfb -displayfd tries every display number.
        try:
            with socket.socket(socket.AF_UNIX) as probe:
                probe.bind(str(temp / "x11-socket-check"))
        except OSError as error:
            raise RuntimeError("This execution sandbox cannot bind Unix sockets; run the developer GUI test in an authorized local host process. " + str(error)) from error
        (temp / "x11-socket-check").unlink()
        photos = temp / ("示例照片" if screenshots else "测试照片")
        photos.mkdir()
        paths, original, expected_times = fixtures(photos, oracle, documentation=bool(screenshots))
        runtime = temp / "standalone"
        runtime.mkdir()
        executable = runtime / "photopinfix"
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        empty_path = temp / "empty-path"
        empty_path.mkdir()
        display_read, display_write = os.pipe()
        logs = [open(artifacts / name, "wb") for name in ("gui-xvfb.log", "gui-stdout.log", "gui-stderr.log")]
        try:
            xserver = subprocess.Popen([str(xvfb), "-displayfd", str(display_write), "-screen", "0", "1440x960x24", "-nolisten", "tcp", "-ac", "-noreset"],
                                       pass_fds=(display_write,), stdout=logs[0], stderr=logs[0])
            os.close(display_write)
            if not select.select([display_read], [], [], 15)[0]:
                raise RuntimeError("Xvfb did not become ready; inspect gui-xvfb.log")
            display_number = os.read(display_read, 128).decode().strip()
            os.close(display_read)
            if not display_number.isdecimal():
                raise RuntimeError(f"Xvfb failed to allocate a display: {display_number!r}")
            display = ":" + display_number
            x = X11(display)
            env = os.environ.copy()
            for key in ("WAYLAND_DISPLAY", "PYTHONPATH", "PYTHONHOME", "PERL5LIB", "PERL5OPT", "CONDA_PREFIX", "VIRTUAL_ENV", "PHOTOPINFIX_TEST_EXIFTOOL", "PHOTOPINFIX_TEST_XVFB"):
                env.pop(key, None)
            env.update(DISPLAY=display, XDG_SESSION_TYPE="x11", WINIT_UNIX_BACKEND="x11", LIBGL_ALWAYS_SOFTWARE="1",
                       __GLX_VENDOR_LIBRARY_NAME="mesa", PATH=str(empty_path), PHOTOPINFIX_PYTHON="/nonexistent/python",
                       PHOTOPINFIX_EXIFTOOL="/nonexistent/exiftool", PHOTOPINFIX_GUI_FOREGROUND="1")
            # Keep the real window process observable for write/exit cleanup.
            # check_gui_launch.py separately verifies the public detached launch.
            env.pop("PHOTOPINFIX_GUI_CHILD", None)
            application = subprocess.Popen([str(executable), "--gui", str(photos)], cwd=runtime, env=env,
                                           stdout=logs[1], stderr=logs[2])
            x.process = application
            x.wait(x.find_window, "native PhotoPinFix window appears")
            x.settled(artifacts / "gui-01-initial.png")
            print(f"Native GUI rendered with an empty PATH: {artifacts / 'gui-01-initial.png'}", flush=True)
            # Skip original GPS and visit both pending automatic photos.
            x.key("bracketright")
            x.settled()
            x.key("bracketright")
            x.settled()
            x.key("bracketleft")
            if screenshots:
                # A quiet loading placeholder is also a stable frame. For the
                # documentation landscape, require an actual, sufficiently
                # large photograph inside the painted preview canvas.
                def landscape_rendered():
                    try:
                        return photo_geometry(x.frame())
                    except RuntimeError:
                        return False

                x.wait(landscape_rendered, "landscape pixels replace the loading placeholder", timeout=30)
            overview = x.settled(artifacts / "gui-overview.png" if screenshots else None)
            overview_geometry = photo_geometry(overview)
            x.key("e")
            x.settled(artifacts / "gui-02-editor.png")
            x.key("a", ("Control_L",))
            x.text("-33.5")
            x.key("Tab")
            x.key("a", ("Control_L",))
            x.text("-70.25")
            x.key("F2")
            x.settled(artifacts / "gui-03-editor-dms.png")
            x.key("F3")
            x.settled(artifacts / "gui-04-editor-hemisphere.png")
            x.key("Return")
            x.settled()
            # A manual assignment stays in missing-photo navigation until it
            # is written. The independent oracle later verifies this photo.
            x.key("bracketright")
            x.settled()
            x.key("bracketleft")
            x.settled(artifacts / "gui-04b-manual-pending.png")
            x.key("Return")
            x.settled(artifacts / "gui-05-confirm-current.png")
            # Enter must cancel the default-focused confirmation, never write.
            x.key("Return")
            x.settled()
            assert not any(photos.glob("*.photopinfix-original")), "Enter unexpectedly confirmed a write"
            x.key("Return")
            x.settled()
            x.key("y")

            def written(index):
                path = paths[index]
                return path.with_name(path.name + ".photopinfix-original").exists() and snapshot(path) == expected_times[path]

            x.wait(lambda: written(1), "current photo write completes and restores file times")
            x.settled(artifacts / "gui-06-current-written.png")
            print("Manual GPS edit and confirmed current-photo write completed.", flush=True)
            x.key("Return", ("Shift_L",))
            x.settled(artifacts / "gui-07-confirm-all.png")
            x.key("y")
            x.wait(lambda: written(2), "remaining automatic photo write completes and restores file times")
            x.settled(artifacts / "gui-08-all-written.png")
            print("Confirmed batch write completed.", flush=True)
            x.key("s")
            x.settled(artifacts / "gui-09-settings.png")
            x.key("Escape")
            x.settled()
            x.key("Right")
            x.settled(artifacts / "gui-10-second-session.png")
            x.key("a")
            x.settled(artifacts / "gui-11-all-sessions.png")
            x.lib.XResizeWindow(x.display, x.window, 880, 640)
            x.lib.XFlush(x.display)
            x.wait(lambda: (x.attributes().width, x.attributes().height) == (880, 640), "minimum-size viewport")
            compact = x.settled(artifacts / "gui-12-minimum-880x640.png")
            compact_geometry = photo_geometry(compact)
            # Compact-layout tabs must be reachable by mouse as well as the
            # keyboard workflows. Locate them in the real capture so header
            # height, window decorations and future layout shifts are harmless.
            tab_points = compact_tab_points(compact)
            x.click(*tab_points[1])
            map_frame = x.settled(artifacts / "gui-12b-minimum-map.png")
            assert_selected_tab(map_frame, tab_points[1])
            assert_map_canvas(map_frame, tab_points[0])
            x.click(*tab_points[2])
            location = x.settled(artifacts / "gui-12c-minimum-location.png")
            assert_selected_tab(location, tab_points[2])
            # Switching to short content must not pull the action bar upward
            # across the library, or leave an unpainted strip at the bottom.
            footer_bytes = compact[0] * 56 * 3
            for name, frame in (("map", map_frame), ("location", location)):
                assert frame[2][-footer_bytes:] == compact[2][-footer_bytes:], \
                    f"Action bar moved or changed after selecting the {name} tab"
            changed = sum(abs(before - after) > 12 for before, after in zip(compact[2], location[2]))
            assert changed > len(compact[2]) * 0.04, "Location tab did not visibly replace the photograph"
            x.click(*tab_points[0])
            restored = x.settled()
            assert_selected_tab(restored, tab_points[0])
            photo_geometry(restored)
            # The fifth photo still has no GPS. Check the entire coordinate
            # form at the minimum window size without writing or staging it.
            for _ in range(4):
                x.key("Down")
            x.key("e")
            x.settled()
            x.key("a", ("Control_L",))
            x.text("31.2")
            x.key("Tab")
            x.key("a", ("Control_L",))
            x.text("121.4")
            x.settled(artifacts / "gui-13-minimum-editor.png")
            x.key("Escape")
            x.settled()
            x.lib.XResizeWindow(x.display, x.window, 1440, 640)
            x.lib.XFlush(x.display)
            x.wait(lambda: (x.attributes().width, x.attributes().height) == (1440, 640), "wide short viewport")
            x.settled(artifacts / "gui-14-wide-short.png")
            x.process = None
            x.key("q", ("Control_L",))
            result = application.wait(timeout=20)
            assert result == 0, f"GUI exited with {result}"
            verify(oracle, paths, original, expected_times)
            if screenshots:
                # Export exact X11 window captures only after the full workflow
                # and independent metadata checks pass. No cropping, retouching
                # or synthetic UI is used for the documentation images.
                screenshots.mkdir(parents=True, exist_ok=True)
                for source, name in (("gui-overview.png", "overview.png"),
                                     ("gui-04-editor-hemisphere.png", "gps-editor.png")):
                    shutil.copyfile(artifacts / source, screenshots / name)
            report = {"executable": binary.name, "empty_path": True, "outside_checkout": True,
                      "native_window": True, "manual_and_auto_writes": True, "all_times_and_inode_preserved": True,
                      "photo_geometry": {"overview": overview_geometry, "compact": compact_geometry},
                      "compact_tab_points": tab_points,
                      "birthtime_available": all(value["created_ns"] is not None for value in expected_times.values()),
                      "screenshots": sorted(path.name for path in artifacts.glob("gui-*.png"))}
            (artifacts / "gui-report.json").write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2), flush=True)
        except Exception:
            if x and x.window and application and application.poll() is None:
                try:
                    png(artifacts / "gui-failure.png", *x.frame())
                except Exception:
                    pass
            try:
                for log in logs:
                    log.flush()
                print_failure_logs(artifacts)
            except Exception:
                # Reporting must not replace the original test failure, even
                # when its output stream is closed or a log is inaccessible.
                pass
            raise
        finally:
            if application and application.poll() is None:
                if x and x.window:
                    try:
                        x.key("Escape")
                        x.key("q", ("Control_L",))
                        x.key("y")
                    except Exception:
                        pass
                try:
                    application.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    application.terminate()
                    try:
                        application.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        application.kill()
                        application.wait()
            if x:
                x.close()
            if xserver:
                xserver.terminate()
                xserver.wait(timeout=5)
            for log in logs:
                log.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", default=str(ROOT / "target/debug/photopinfix"))
    parser.add_argument("--artifacts", default=str(ROOT / "target/test-artifacts"))
    parser.add_argument("--screenshots", metavar="DIRECTORY", help="Export verified, unmodified overview.png and gps-editor.png window captures for documentation")
    options = parser.parse_args()
    try:
        run(options)
    except Exception as error:
        print(f"GUI integration test failed: {error}", file=sys.stderr, flush=True)
        raise
