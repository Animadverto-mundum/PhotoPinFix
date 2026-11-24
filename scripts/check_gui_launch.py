#!/usr/bin/env python3
"""Verify detached --gui startup and the welcome-folder flow on real Linux/X11.

Developer tooling only. Reuses check_gui's X11 driver and independent fixture
oracle. The copied app has an empty PATH, starts from a real controlling PTY,
and must survive that PTY closing before any mouse/keyboard interaction.
"""

import argparse
import ctypes as C
import errno
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

from check_gui import Oracle, ROOT, X11, fixtures, png, preserved_read, snapshot


class Child:
    """Track a forked or reparented process without depending on a shell."""

    def __init__(self, pid):
        self.pid = pid
        self.returncode = None

    def poll(self):
        if self.returncode is None:
            waited, status = os.waitpid(self.pid, os.WNOHANG)
            if waited:
                self.returncode = os.waitstatus_to_exitcode(status)
        return self.returncode


class LaunchX11(X11):
    def unicode_text(self, text):
        """Type Unicode directly with a temporary keymap on our isolated Xvfb."""
        self.lib.XDisplayKeycodes.argtypes = [C.c_void_p, C.POINTER(C.c_int), C.POINTER(C.c_int)]
        self.lib.XGetKeyboardMapping.argtypes = [C.c_void_p, C.c_ubyte, C.c_int, C.POINTER(C.c_int)]
        self.lib.XGetKeyboardMapping.restype = C.POINTER(C.c_ulong)
        self.lib.XChangeKeyboardMapping.argtypes = [C.c_void_p, C.c_int, C.c_int, C.POINTER(C.c_ulong), C.c_int]
        self.lib.XSync.argtypes = [C.c_void_p, C.c_int]
        low, high, count = C.c_int(), C.c_int(), C.c_int()
        self.lib.XDisplayKeycodes(self.display, C.byref(low), C.byref(high))
        code = high.value
        saved = self.lib.XGetKeyboardMapping(self.display, code, 1, C.byref(count))
        if not saved or count.value < 1:
            raise RuntimeError("Could not preserve the isolated X11 keymap")
        try:
            for char in text:
                symbol = ord(char) if ord(char) < 0x100 else 0x01000000 | ord(char)
                mapping = (C.c_ulong * count.value)(*([symbol] * count.value))
                self.lib.XChangeKeyboardMapping(self.display, code, count.value, mapping, 1)
                self.lib.XSync(self.display, False)
                # MappingNotify must reach winit before the injected keypress.
                self.events(0.06)
                self.xtest.XTestFakeKeyEvent(self.display, code, 1, 0)
                self.xtest.XTestFakeKeyEvent(self.display, code, 0, 0)
                self.lib.XFlush(self.display)
                self.events(0.06)
        finally:
            self.lib.XChangeKeyboardMapping(self.display, code, count.value, saved, 1)
            self.lib.XSync(self.display, False)
            self.lib.XFree(saved)


def primary_center(frame, central=False):
    """Find the painted primary action, not a fragile text-dependent position."""
    width, height, pixels = frame
    bands, active = [], []
    for y in range(height // 3, height):
        xs = [x for x in range(width) if pixels[(y * width + x) * 3:(y * width + x) * 3 + 3] == b"\x1e\x74\x5b"]
        valid = len(xs) >= 85 and xs[-1] - xs[0] >= 99
        if valid and central:
            valid = width * 0.30 < (xs[0] + xs[-1]) / 2 < width * 0.70
        if valid:
            active.append((xs[0], xs[-1], y))
        elif active:
            if y - active[-1][2] < 15:
                continue  # White button text can interrupt a few scanlines.
            bands.append(active)
            active = []
    if active:
        bands.append(active)
    candidates = [band for band in bands if band[-1][2] - band[0][2] >= 24]
    if not candidates:
        return None
    band = candidates[-1]
    return ((min(row[0] for row in band) + max(row[1] for row in band)) // 2,
            (band[0][2] + band[-1][2]) // 2)


def photographic_pixels(frame):
    width, height, pixels = frame
    colors = {pixels[(y * width + x) * 3:(y * width + x) * 3 + 3]
              for y in range(height // 4, height * 4 // 5, 3)
              for x in range(width // 3, width * 3 // 4, 3)}
    return len(colors) > 1800


def run(args):
    if sys.platform != "linux":
        raise RuntimeError("This real PTY/X11 acceptance test runs on Linux")
    binary = Path(args.binary).resolve()
    if not binary.is_file():
        raise RuntimeError("Build the PhotoPinFix executable before running this test")
    artifacts = Path(args.artifacts).resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    override = os.environ.get("PHOTOPINFIX_TEST_XVFB")
    xvfb = Path(override).resolve() if override else ROOT / "tools/gui-test/root/usr/bin/Xvfb"
    if not xvfb.is_file() and not override:
        xvfb = Path(shutil.which("Xvfb") or "/missing-Xvfb")
    if not xvfb.is_file():
        raise RuntimeError("Provide developer Xvfb via PHOTOPINFIX_TEST_XVFB")
    oracle = Oracle()
    libc = C.CDLL(None, use_errno=True)
    # Orphaned detached children reparent here, so successful cleanup has an
    # observed exit status instead of an unverified process-disappeared guess.
    if libc.prctl(36, 1, 0, 0, 0):  # PR_SET_CHILD_SUBREAPER
        raise RuntimeError("Cannot enable child reaping for detached GUI test")
    launcher = application = xserver = x = None
    master = None
    with tempfile.TemporaryDirectory(prefix="photopinfix-launch-") as temporary:
        temp = Path(temporary)
        with socket.socket(socket.AF_UNIX) as probe:
            probe.bind(str(temp / "socket-check"))
        (temp / "socket-check").unlink()
        photos = temp / "中文 照片样本"
        photos.mkdir()
        paths, original, expected_times = fixtures(photos, oracle)
        runtime, empty_path = temp / "standalone", temp / "empty-path"
        runtime.mkdir()
        empty_path.mkdir()
        executable = runtime / "photopinfix"
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        # No-directory GUI must show the welcome view, even if cwd has images.
        png(runtime / "cwd-must-not-open.png", 4, 4, bytes((255, 0, 255)) * 16)
        read_fd, write_fd = os.pipe()
        with (artifacts / "gui-launch-xvfb.log").open("wb") as server_log:
            try:
                xserver = subprocess.Popen([str(xvfb), "-displayfd", str(write_fd), "-screen", "0", "1440x960x24",
                                           "-nolisten", "tcp", "-ac", "-noreset"], pass_fds=(write_fd,),
                                          stdout=server_log, stderr=server_log)
                os.close(write_fd)
                if not select.select([read_fd], [], [], 15)[0]:
                    raise RuntimeError("Xvfb did not become ready")
                number = os.read(read_fd, 128).decode().strip()
                os.close(read_fd)
                if not number.isdecimal():
                    raise RuntimeError("Xvfb did not provide a display number")
                display = ":" + number
                x = LaunchX11(display)
                env = os.environ.copy()
                for name in ("WAYLAND_DISPLAY", "PYTHONPATH", "PYTHONHOME", "PERL5LIB", "PERL5OPT", "CONDA_PREFIX", "VIRTUAL_ENV",
                             "PHOTOPINFIX_TEST_EXIFTOOL", "PHOTOPINFIX_TEST_XVFB", "PHOTOPINFIX_GUI_FOREGROUND", "PHOTOPINFIX_GUI_CHILD"):
                    env.pop(name, None)
                env.update(DISPLAY=display, XDG_SESSION_TYPE="x11", WINIT_UNIX_BACKEND="x11", LIBGL_ALWAYS_SOFTWARE="1",
                           __GLX_VENDOR_LIBRARY_NAME="mesa", PATH=str(empty_path), LANG="C.UTF-8", LC_ALL="C.UTF-8",
                           HOME=str(runtime), USERPROFILE=str(runtime))
                pid, master = pty.fork()
                if pid == 0:
                    try:
                        os.chdir(runtime)
                        os.execve(executable, [str(executable), "--gui"], env)
                    except BaseException as error:
                        # Never run the parent's Xvfb cleanup from the forked
                        # child if exec fails.
                        os.write(2, ("GUI test exec failed: " + type(error).__name__ + "\n").encode())
                        os._exit(127)
                launcher = Child(pid)
                output = bytearray()

                def drain_terminal():
                    while select.select([master], [], [], 0)[0]:
                        try:
                            data = os.read(master, 8192)
                            if not data:
                                break
                            output.extend(data)
                            if len(output) > 1024 * 1024:
                                raise RuntimeError("Unexpectedly large GUI launcher output")
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            break

                def launcher_done():
                    drain_terminal()
                    done = launcher.poll() is not None
                    if done:
                        # Ready output can arrive between the first read and
                        # waitpid observing exit. Do not lose that final chunk.
                        drain_terminal()
                        (artifacts / "gui-launch-parent.log").write_bytes(output)
                    return done

                x.wait(launcher_done, "GUI launcher exits after the ready handshake", timeout=30)
                assert launcher.returncode == 0, "Detached GUI launcher failed"
                match = re.search(rb"PID\s+(\d+)", output)
                assert match, "Launcher did not identify the detached GUI process"
                application = Child(int(match[1]))
                assert application.pid != launcher.pid, "Launcher never detached"
                x.process = application
                assert os.getsid(application.pid) == application.pid, "GUI remains in the terminal session"
                # /proc stat's comm field can contain spaces and parentheses.
                fields = Path(f"/proc/{application.pid}/stat").read_text().rsplit(") ", 1)[1].split()
                assert int(fields[4]) == 0, "GUI still has a controlling terminal"
                os.close(master)
                master = None
                x.wait(x.find_window, "detached native window survives its PTY closing")
                frame = x.settled(artifacts / "gui-launch-01-welcome.png")
                button = primary_center(frame, central=True)
                assert button, "No-directory GUI did not render the welcome folder action"
                print("PASS: launcher exits 0; detached native window survives closing its real controlling PTY", flush=True)
                x.click(*button)
                picker_frame = x.settled(artifacts / "gui-launch-02-folder-picker.png")
                assert hashlib.sha256(picker_frame[2]).digest() != hashlib.sha256(frame[2]).digest(), "Welcome action did not open the folder picker"
                x.key("a", ("Control_L",))
                x.unicode_text(str(photos))
                x.key("Return")
                frame = x.settled(artifacts / "gui-launch-03-unicode-folder.png")
                button = primary_center(frame)
                assert button, "Folder picker did not expose its confirmation action"
                x.click(*button)
                x.wait(lambda: photographic_pixels(x.frame()), "chosen Chinese/spaces folder loads an actual photo", timeout=30)
                x.settled(artifacts / "gui-launch-04-photos.png")
                assert application.poll() is None, "Detached GUI stopped during folder selection"
                x.process = None
                x.key("q", ("Control_L",))
                x.wait(lambda: application.poll() is not None, "detached GUI exits cleanly", timeout=20)
                assert application.returncode == 0, "Detached GUI did not exit successfully"
                for path in paths:
                    assert snapshot(path) == expected_times[path], f"File times changed: {path.name}"
                    assert preserved_read(path, path.read_bytes) == original[path], f"Browsing modified image: {path.name}"
                report = {"executable": binary.name, "launcher_exit": 0, "detached_session": True,
                          "terminal_closed_before_interaction": True, "welcome_without_directory": True,
                          "folder_picker_mouse_open_and_confirm": True, "unicode_and_spaces_path": True,
                          "actual_photo_preview": True, "empty_path": True, "outside_checkout": True,
                          "file_times_and_bytes_preserved": True, "gui_exit": 0}
                (artifacts / "gui-launch-report.json").write_text(json.dumps(report, indent=2) + "\n")
                print(json.dumps(report, indent=2), flush=True)
            except Exception:
                if x and x.window and application and application.poll() is None:
                    try:
                        png(artifacts / "gui-launch-failure.png", *x.frame())
                    except Exception:
                        pass
                raise
            finally:
                if master is not None:
                    os.close(master)
                if application and application.poll() is None:
                    if x and x.window:
                        try:
                            x.key("Escape")
                            x.key("q", ("Control_L",))
                            x.key("y")
                            x.process = None
                            x.wait(lambda: application.poll() is not None, "test cleanup closes detached GUI", timeout=5)
                        except Exception:
                            pass
                    if application.poll() is None:
                        os.kill(application.pid, signal.SIGTERM)
                        os.waitpid(application.pid, 0)
                if launcher and launcher.poll() is None:
                    os.kill(launcher.pid, signal.SIGTERM)
                    os.waitpid(launcher.pid, 0)
                if x:
                    x.close()
                if xserver:
                    xserver.terminate()
                    xserver.wait(timeout=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", default=str(ROOT / "target/debug/photopinfix"))
    parser.add_argument("--artifacts", default=str(ROOT / "target/test-artifacts"))
    try:
        run(parser.parse_args())
    except Exception as error:
        print(f"GUI launch acceptance failed: {error}", file=sys.stderr, flush=True)
        raise
