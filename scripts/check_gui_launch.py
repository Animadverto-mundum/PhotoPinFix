#!/usr/bin/env python3
"""Verify detached --gui startup and the welcome-folder flow on real Linux/X11.

Developer tooling only. Reuses check_gui's X11 driver and independent fixture
oracle. The copied app has an empty PATH, starts from a real controlling PTY,
and must survive that PTY closing before any mouse/keyboard interaction.
"""

import argparse
import ctypes as C
import errno
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
from xml.sax.saxutils import escape

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
    def __init__(self, display_name):
        super().__init__(display_name)
        # Window discovery races naturally with native dialogs closing. Turn
        # Xlib's default process exit into the normal failed-return path, so
        # cleanup never masks the original test assertion with BadWindow.
        self._error_callback_type = C.CFUNCTYPE(C.c_int, C.c_void_p, C.c_void_p)
        self._error_callback = self._error_callback_type(lambda _display, _event: 0)
        self.lib.XSetErrorHandler.argtypes = [self._error_callback_type]
        self.lib.XSetErrorHandler(self._error_callback)
        self.lib.XSync.argtypes = [C.c_void_p, C.c_int]
        self.lib.XInternAtom.argtypes = [C.c_void_p, C.c_char_p, C.c_int]
        self.lib.XInternAtom.restype = C.c_ulong
        self.lib.XGetWindowProperty.argtypes = [C.c_void_p, C.c_ulong, C.c_ulong, C.c_long, C.c_long,
                                                C.c_int, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_int),
                                                C.POINTER(C.c_ulong), C.POINTER(C.c_ulong), C.POINTER(C.POINTER(C.c_ubyte))]
        self.native_pid = None

    def click(self, x, y):
        # A private Xvfb has no window manager to transfer keyboard focus when
        # a native modal closes. Establish focus before delivering the click.
        self.lib.XSetInputFocus(self.display, self.window, 1, 0)
        self.lib.XSync(self.display, False)
        self.events(0.08)
        super().click(x, y)

    def property(self, window, name):
        atom = self.lib.XInternAtom(self.display, name.encode(), False)
        kind, length, remaining = C.c_ulong(), C.c_ulong(), C.c_ulong()
        format_ = C.c_int()
        data = C.POINTER(C.c_ubyte)()
        status = self.lib.XGetWindowProperty(self.display, window, atom, 0, 4096, False, 0,
                                             C.byref(kind), C.byref(format_), C.byref(length), C.byref(remaining), C.byref(data))
        try:
            if status or not data:
                return None
            if format_.value == 8:
                return C.string_at(data, length.value)
            if format_.value == 32 and length.value:
                return C.cast(data, C.POINTER(C.c_ulong))[0]
            return None
        finally:
            if data:
                self.lib.XFree(data)

    def native_dialog(self, main):
        root, parent, children, count = C.c_ulong(), C.c_ulong(), C.POINTER(C.c_ulong)(), C.c_uint()
        if not self.lib.XQueryTree(self.display, self.root, C.byref(root), C.byref(parent), C.byref(children), C.byref(count)):
            return None
        try:
            for index in range(count.value):
                window = children[index]
                if window == main:
                    continue
                title = self.property(window, "_NET_WM_NAME") or b""
                pid = self.property(window, "_NET_WM_PID")
                if b"PhotoPinFix" in title or (self.native_pid is not None and pid == self.native_pid):
                    try:
                        attrs = self.attributes(window)
                    except RuntimeError:
                        continue  # It closed between XQueryTree and inspection.
                    if attrs.map_state == 2 and attrs.width > 300 and attrs.height > 200:
                        return window
        finally:
            if children:
                self.lib.XFree(children)
        return None



class NativePortalServices:
    """Real GTK portal services on a private test bus, never a mocked answer."""

    def __init__(self, temporary, display, artifacts):
        self.processes, self.logs = [], []
        self.directory = temporary
        runtime = temporary / "portal-runtime"
        runtime.mkdir(mode=0o700)
        address = "unix:path=" + str(runtime / "bus")
        self.env = os.environ.copy()
        self.env.update(DISPLAY=display, DBUS_SESSION_BUS_ADDRESS=address, XDG_RUNTIME_DIR=str(runtime),
                        XDG_CURRENT_DESKTOP="GNOME", GDK_BACKEND="x11", GTK_USE_PORTAL="0",
                        HOME=str(temporary), XDG_CONFIG_HOME=str(temporary / "config"),
                        GSETTINGS_BACKEND="memory", NO_AT_BRIDGE="1")
        self.env.pop("WAYLAND_DISPLAY", None)
        fonts, cache = runtime / "fonts", runtime / "font-cache"
        fonts.mkdir()
        cache.mkdir()
        source_font = ROOT / "assets/fonts/NotoSansSC-Regular.otf"
        preserved_read(source_font, lambda: shutil.copyfile(source_font, fonts / source_font.name))
        fontconfig = runtime / "fonts.conf"
        fontconfig.write_text(
            '<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">'
            '<fontconfig><include ignore_missing="yes">/etc/fonts/fonts.conf</include>'
            f'<dir>{escape(str(fonts))}</dir><cachedir>{escape(str(cache))}</cachedir></fontconfig>\n'
        )
        self.env.update(FONTCONFIG_FILE=str(fontconfig), XDG_CACHE_HOME=str(cache))

        def executable(override, name):
            if os.environ.get(override):
                path = Path(os.environ[override]).resolve()
                if path.is_file():
                    return path
            for folder in (ROOT / "tools/gui-test/portal-root/usr/libexec", ROOT / "tools/gui-test/portal-root/usr/lib",
                           Path("/usr/libexec"), Path("/usr/lib")):
                if (folder / name).is_file():
                    return folder / name
            found = shutil.which(name)
            if found:
                return Path(found)
            raise RuntimeError(f"Real native picker acceptance requires developer {name}; no fake/skip fallback is permitted")

        portal = executable("PHOTOPINFIX_TEST_PORTAL", "xdg-desktop-portal")
        gtk = executable("PHOTOPINFIX_TEST_PORTAL_GTK", "xdg-desktop-portal-gtk")
        # Extracted test packages keep their real .portal definitions here.
        share = portal.parent.parent / "share"
        self.env["XDG_DATA_DIRS"] = str(share) + ":/usr/local/share:/usr/share"
        self.env["XDG_DESKTOP_PORTAL_DIR"] = str(share / "xdg-desktop-portal/portals")
        libraries = portal.parent.parent / "lib/x86_64-linux-gnu"
        if libraries.is_dir():
            self.env["LD_LIBRARY_PATH"] = str(libraries) + ":" + self.env.get("LD_LIBRARY_PATH", "")
        config = runtime / "bus.conf"
        config.write_text('<busconfig><type>session</type><listen>' + address + '</listen><auth>EXTERNAL</auth>'
                          '<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/>'
                          '<allow own="*"/></policy></busconfig>')
        bus = shutil.which("dbus-daemon")
        if not bus:
            raise RuntimeError("Native picker acceptance needs a developer D-Bus daemon")
        try:
            self.start([bus, "--nofork", "--config-file=" + str(config)], artifacts / "gui-launch-dbus.log")
            deadline = time.monotonic() + 10
            while not (runtime / "bus").exists():
                self.check()
                if time.monotonic() >= deadline:
                    raise RuntimeError("Private D-Bus socket did not become ready")
                time.sleep(0.05)
            self.start([str(gtk)], artifacts / "gui-launch-portal-gtk.log")
            self.gtk_pid = self.processes[-1].pid
            self.start([str(portal)], artifacts / "gui-launch-portal.log")
            # These are service startup checks. The actual UI still has to
            # appear and return a real user-selected path through D-Bus.
            gdbus = shutil.which("gdbus")
            if not gdbus:
                raise RuntimeError("Native picker acceptance needs developer gdbus")
            deadline = time.monotonic() + 15
            while True:
                self.check()
                result = subprocess.run([gdbus, "call", "--session", "--dest", "org.freedesktop.portal.Desktop",
                                         "--object-path", "/org/freedesktop/portal/desktop", "--method", "org.freedesktop.DBus.Properties.Get",
                                         "org.freedesktop.portal.FileChooser", "version"], env=self.env, capture_output=True, timeout=3)
                if result.returncode == 0:
                    version = re.search(rb"uint32\s+(\d+)", result.stdout)
                    if not version or int(version[1]) < 3:
                        raise RuntimeError("Native directory acceptance requires FileChooser API v3 or newer (portal >= 1.8); this portal only supports file selection")
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError("Real FileChooser portal did not become ready; inspect portal logs")
                time.sleep(0.1)
        except Exception:
            self.close()
            raise

    def start(self, command, path):
        log = path.open("wb")
        self.logs.append(log)
        self.processes.append(subprocess.Popen(command, cwd=self.directory, env=self.env, stdout=log, stderr=log))

    def check(self):
        if any(process.poll() is not None for process in self.processes):
            raise RuntimeError("A private portal service exited; inspect native picker logs")

    def close(self):
        for process in reversed(self.processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        for log in self.logs:
            log.close()


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
    launcher = application = xserver = x = portal = main_window = None
    master = None
    with tempfile.TemporaryDirectory(prefix="photopinfix-launch-") as temporary:
        temp = Path(temporary)
        with socket.socket(socket.AF_UNIX) as probe:
            probe.bind(str(temp / "socket-check"))
        (temp / "socket-check").unlink()
        photos = temp / "Photos 中文 照片样本"
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
                portal = NativePortalServices(temp, display, artifacts)
                x.native_pid = portal.gtk_pid
                env = os.environ.copy()
                for name in ("WAYLAND_DISPLAY", "PYTHONPATH", "PYTHONHOME", "PERL5LIB", "PERL5OPT", "CONDA_PREFIX", "VIRTUAL_ENV",
                             "PHOTOPINFIX_TEST_EXIFTOOL", "PHOTOPINFIX_TEST_XVFB", "PHOTOPINFIX_GUI_FOREGROUND", "PHOTOPINFIX_GUI_CHILD"):
                    env.pop(name, None)
                env.update(DISPLAY=display, XDG_SESSION_TYPE="x11", WINIT_UNIX_BACKEND="x11", LIBGL_ALWAYS_SOFTWARE="1",
                           __GLX_VENDOR_LIBRARY_NAME="mesa", PATH=str(empty_path), LANG="C.UTF-8", LC_ALL="C.UTF-8",
                           HOME=str(runtime), USERPROFILE=str(runtime))
                env.update(DBUS_SESSION_BUS_ADDRESS=portal.env["DBUS_SESSION_BUS_ADDRESS"],
                           XDG_RUNTIME_DIR=portal.env["XDG_RUNTIME_DIR"], XDG_CURRENT_DESKTOP="GNOME")
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
                main_window = x.window
                x.click(*button)
                native = x.wait(lambda: x.native_dialog(main_window), "real GTK portal folder dialog")
                x.window = native
                width, height, _ = x.settled(artifacts / "gui-launch-02-native-folder-picker.png")
                x.click(width - 145, height - 23)  # Native Cancel action.
                x.wait(lambda: x.native_dialog(main_window) is None, "native picker cancellation")
                x.window = main_window
                frame = x.settled(artifacts / "gui-launch-02b-cancelled-welcome.png")
                button = primary_center(frame, central=True)
                assert button, "Cancelling the native picker unexpectedly left the welcome page"
                x.key("o", ("Control_L",))
                x.window = x.wait(lambda: x.native_dialog(main_window), "native picker reopens after cancellation")
                # Initialize GTK's directory browsing mode before entering a
                # location; its empty Recent view has no completion model.
                x.click(60, 60)  # Native Home sidebar action.
                x.settled()
                x.click(300, 80)  # Focus the native folder list.
                x.text("photos")  # GTK type-to-search selects the real Unicode folder.
                width, height, _ = x.settled(artifacts / "gui-launch-03-unicode-folder.png")
                x.click(width - 50, height - 23)  # Native Select action.
                x.wait(lambda: x.native_dialog(main_window) is None, "native folder selection closes")
                x.window = main_window
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
                          "os_native_folder_picker": "XDG portal / GTK", "real_native_selection": True,
                          "native_cancel_and_reopen": True,
                          "unicode_and_spaces_path": True,
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
                            x.process = None
                            x.key("Escape")
                            if main_window:
                                x.window = main_window
                            x.key("q", ("Control_L",))
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
                if portal:
                    portal.close()
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
