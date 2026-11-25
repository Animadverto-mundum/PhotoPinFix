#!/usr/bin/env python3
"""Capture real Windows/macOS GUI windows for CI review (developer tooling).

No screenshots are synthesized. Permission/interactive-desktop failures fail
this step and leave a report; a missing image is never reported as a success.
Linux uses check_gui.py/check_gui_launch.py with their private X11 desktop.
"""
import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

from PIL import Image, ImageChops, ImageGrab
from check_gui import Oracle, ROOT, fixtures, preserved_read, snapshot
from gui_visual import compact_selected_tab as selected_tab_geometry, compact_tab_points, photo_geometry


def windows_test_opengl(directory, destination, environment):
    """Copy only manifest-verified CI renderer DLLs beside the disposable exe."""
    if sys.platform != 'win32':
        raise RuntimeError('--windows-opengl-dir is only supported by Windows capture tests')
    directory = Path(directory).resolve()
    manifest = json.loads((directory / 'manifest.json').read_text(encoding='utf-8'))
    expected = {'schema_version': 1, 'purpose': 'ci-software-opengl',
                'architecture': 'x86_64', 'driver': 'llvmpipe'}
    if any(manifest.get(key) != value for key, value in expected.items()):
        raise RuntimeError('Unexpected CI OpenGL manifest purpose, architecture or driver')
    files = manifest.get('files')
    if not isinstance(files, dict) or 'opengl32.dll' not in files:
        raise RuntimeError('CI OpenGL manifest is missing opengl32.dll')

    def digest(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').hexdigest()

    for name, expected_hash in files.items():
        if not isinstance(name, str) or Path(name).name != name or '/' in name or '\\' in name or not name.lower().endswith('.dll'):
            raise RuntimeError('CI OpenGL manifest contains a non-DLL or non-basename entry')
        if not isinstance(expected_hash, str) or len(expected_hash) != 64 or any(char not in '0123456789abcdef' for char in expected_hash):
            raise RuntimeError('CI OpenGL manifest contains an invalid SHA256')
        source = (directory / name).resolve()
        if source.parent != directory or not source.is_file() or digest(source) != expected_hash:
            raise RuntimeError('CI OpenGL DLL checksum or location verification failed: ' + name)
        shutil.copyfile(source, destination / name)
        if digest(destination / name) != expected_hash:
            raise RuntimeError('CI OpenGL DLL changed while copying: ' + name)
    environment.update(GALLIUM_DRIVER='llvmpipe', LIBGL_ALWAYS_SOFTWARE='1',
                       GLUTIN_WGL_OPENGL_DLL=str((destination / 'opengl32.dll').resolve()))
    return {'scope': 'CI capture only; DLLs are not part of the application package',
            'requested_driver': 'llvmpipe', 'version': manifest.get('version'),
            'archive_sha256': manifest.get('archive_sha256'), 'files': files}


class PortableOracle(Oracle):
    def run(self, *args):
        if sys.platform != 'win32':
            return super().run(*args)
        # Windows Perl's argv may use the active ANSI code page. ExifTool's
        # UTF-8 argument-file interface carries Chinese fixture paths losslessly.
        if any('\n' in value or '\r' in value for value in args):
            raise ValueError('Fixture oracle arguments must be single lines')
        result = subprocess.run(self.command + ['-charset', 'filename=UTF8', '-@', '-'],
                                input=('\n'.join(args) + '\n').encode('utf-8'),
                                capture_output=True, check=True, timeout=30)
        return result.stdout


class WindowsDesktop:
    def __init__(self, pid, temporary):
        from ctypes import wintypes as W
        self.pid = pid
        self.main_window = None
        self.preparation = {}
        self.u = C.WinDLL('user32', use_last_error=True)
        self.W = W
        self.callback = C.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)
        self.u.EnumWindows.argtypes = [self.callback, W.LPARAM]
        self.u.GetWindowThreadProcessId.argtypes = [W.HWND, C.POINTER(W.DWORD)]
        self.u.GetWindowRect.argtypes = [W.HWND, C.POINTER(W.RECT)]
        self.u.GetClientRect.argtypes = [W.HWND, C.POINTER(W.RECT)]
        self.u.ClientToScreen.argtypes = [W.HWND, C.POINTER(W.POINT)]
        self.u.GetDpiForWindow.argtypes = [W.HWND]
        self.u.GetDpiForWindow.restype = W.UINT
        self.u.SetCursorPos.argtypes = [C.c_int, C.c_int]
        self.u.IsWindowVisible.argtypes = [W.HWND]
        self.u.SetForegroundWindow.argtypes = [W.HWND]
        self.u.ShowWindow.argtypes = [W.HWND, C.c_int]
        self.u.MoveWindow.argtypes = [W.HWND, C.c_int, C.c_int, C.c_int, C.c_int, W.BOOL]
        self.u.GetForegroundWindow.restype = W.HWND
        self.u.SystemParametersInfoW.argtypes = [W.UINT, W.UINT, C.c_void_p, W.UINT]
        self.u.SystemParametersInfoW.restype = W.BOOL
        try:
            self.u.SetProcessDPIAware()
        except AttributeError:
            pass
        work = self.work_area()
        if work.right - work.left < 880 or work.bottom - work.top < 640:
            raise RuntimeError('Windows runner has no usable interactive desktop')

    def work_area(self):
        # SPI_GETWORKAREA excludes the taskbar and desktop toolbars. Its RECT
        # is in physical pixels, independently of the caller's DPI awareness.
        # https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-systemparametersinfow
        work = self.W.RECT()
        if not self.u.SystemParametersInfoW(0x0030, 0, C.byref(work), 0):
            raise RuntimeError('Cannot read the Windows desktop work area')
        if work.right <= work.left or work.bottom <= work.top:
            raise RuntimeError('Windows runner has an empty desktop work area')
        return work

    def outer_size(self, window, width, height):
        outer, inner = self.W.RECT(), self.W.RECT()
        if not self.u.GetWindowRect(window, C.byref(outer)) or not self.u.GetClientRect(window, C.byref(inner)):
            raise RuntimeError('Cannot measure a missing Windows window')
        dpi = self.u.GetDpiForWindow(window)
        if not dpi:
            raise RuntimeError('Cannot measure the Windows window DPI')
        desired = (round(width * dpi / 96.0), round(height * dpi / 96.0))
        total = (desired[0] + outer.right - outer.left - (inner.right - inner.left),
                 desired[1] + outer.bottom - outer.top - (inner.bottom - inner.top))
        return total, desired

    def windows(self):
        found = []
        @self.callback
        def collect(window, _):
            process = self.W.DWORD()
            self.u.GetWindowThreadProcessId(window, C.byref(process))
            rect = self.W.RECT()
            if process.value == self.pid and self.u.IsWindowVisible(window) and self.u.GetWindowRect(window, C.byref(rect)):
                if rect.right - rect.left > 300 and rect.bottom - rect.top > 200:
                    found.append(int(window))
            return True
        self.u.EnumWindows(collect, 0)
        return found

    def focus(self, window):
        self.u.ShowWindow(window, 9)
        # Fit the whole real window above the taskbar, without cropping the
        # capture or reducing the main client viewport below its supported size.
        work = self.work_area()
        width, height = work.right - work.left, work.bottom - work.top
        minimum = (0, 0)
        if window == self.main_window:
            minimum, _ = self.outer_size(window, 880, 640)
            if minimum[0] > width or minimum[1] > height:
                raise RuntimeError('Windows work area cannot fit the minimum client viewport at its current DPI')
        rect = self.W.RECT()
        if not self.u.GetWindowRect(window, C.byref(rect)):
            raise RuntimeError('Cannot focus a missing Windows window')
        fitted = (max(minimum[0], min(rect.right - rect.left, width)),
                  max(minimum[1], min(rect.bottom - rect.top, height)))
        if (rect.left < work.left or rect.top < work.top or rect.right > work.right or rect.bottom > work.bottom
                or fitted != (rect.right - rect.left, rect.bottom - rect.top)):
            if not self.u.MoveWindow(window, work.left, work.top, *fitted, True):
                raise RuntimeError('Windows refused to fit the real window inside its desktop work area')
        self.u.SetForegroundWindow(window)
        time.sleep(0.15)
        if int(self.u.GetForegroundWindow() or 0) != window:
            raise RuntimeError('Windows refused foreground control; real UI input cannot be verified')

    def native_dialog(self, main):
        return next((window for window in self.windows() if window != main), None)

    def prepare(self, window):
        self.main_window = window
        work = self.work_area()
        self.preparation = {
            'dpi': self.u.GetDpiForWindow(window),
            'work_area': [work.left, work.top, work.right, work.bottom],
            'before': self.window_geometry(window),
        }
        self.focus(window)
        self.preparation['after'] = self.window_geometry(window)

    def window_geometry(self, window):
        outer, inner = self.W.RECT(), self.W.RECT()
        if not self.u.GetWindowRect(window, C.byref(outer)) or not self.u.GetClientRect(window, C.byref(inner)):
            raise RuntimeError('Cannot measure the Windows window geometry')
        origin = self.W.POINT(inner.left, inner.top)
        if not self.u.ClientToScreen(window, C.byref(origin)):
            raise RuntimeError('Cannot locate the Windows client viewport on screen')
        return {'outer': [outer.left, outer.top, outer.right, outer.bottom],
                'client': [origin.x, origin.y, origin.x + inner.right - inner.left,
                           origin.y + inner.bottom - inner.top]}

    def resize(self, window, width, height):
        (total_width, total_height), desired = self.outer_size(window, width, height)
        work = self.work_area()
        if total_width > work.right - work.left or total_height > work.bottom - work.top:
            raise RuntimeError('Windows work area cannot fit the minimum client viewport at its current DPI')
        if not self.u.MoveWindow(window, work.left, work.top, total_width, total_height, True):
            raise RuntimeError('Windows refused to resize the real window')
        time.sleep(0.2)
        inner = self.W.RECT()
        if not self.u.GetClientRect(window, C.byref(inner)):
            raise RuntimeError('Cannot measure the resized Windows client viewport')
        if (inner.right - inner.left, inner.bottom - inner.top) != desired:
            raise RuntimeError('Windows did not apply the requested compact client viewport')

    def image_scale(self, window, image):
        rect = self.W.RECT()
        if not self.u.GetWindowRect(window, C.byref(rect)):
            raise RuntimeError('Cannot measure a missing Windows window')
        return image.width / (rect.right - rect.left) * self.u.GetDpiForWindow(window) / 96.0

    def click_capture(self, window, image, x, y):
        self.focus(window)
        rect = self.W.RECT()
        if not self.u.GetWindowRect(window, C.byref(rect)):
            raise RuntimeError('Cannot locate a missing Windows window')
        self.u.SetCursorPos(round(rect.left + x * (rect.right - rect.left) / image.width),
                            round(rect.top + y * (rect.bottom - rect.top) / image.height))
        self.u.mouse_event(0x0002, 0, 0, 0, 0)
        self.u.mouse_event(0x0004, 0, 0, 0, 0)
        time.sleep(0.2)

    def key(self, name, command=False, window=None):
        if window is not None:
            self.focus(window)
        key = {'down': 0x28, 'escape': 0x1b}.get(name, ord(name.upper()[0]))
        if command:
            self.u.keybd_event(0x11, 0, 0, 0)
        self.u.keybd_event(key, 0, 0, 0)
        self.u.keybd_event(key, 0, 2, 0)
        if command:
            self.u.keybd_event(0x11, 0, 2, 0)
        time.sleep(0.2)

    def capture(self, window):
        rect = self.W.RECT()
        if not self.u.GetWindowRect(window, C.byref(rect)):
            raise RuntimeError('Native Windows window no longer exists')
        work = self.work_area()
        if rect.left < work.left or rect.top < work.top or rect.right > work.right or rect.bottom > work.bottom:
            raise RuntimeError('Native Windows window exceeds the desktop work area; refusing an obscured or clipped screenshot')
        return ImageGrab.grab(bbox=(rect.left, rect.top, rect.right, rect.bottom), all_screens=True).convert('RGB')


class AppleScriptError(RuntimeError):
    """A real osascript failure, retaining its bounded diagnostic and code."""

    def __init__(self, returncode, stderr):
        self.returncode = returncode
        detail = stderr.decode('utf-8', errors='replace')[-2000:].strip()
        match = re.search(r'\((-?\d+)\)\s*$', detail)
        self.error_code = int(match.group(1)) if match else None
        code = f', AppleScript {self.error_code}' if self.error_code is not None else ''
        super().__init__(f'macOS System Events failed (exit {returncode}{code}): {detail or "(empty stderr)"}')


class MacDesktop:
    def __init__(self, pid, temporary):
        import Quartz
        import AppKit
        self.q, self.appkit, self.pid = Quartz, AppKit, pid
        self.capture_path = temporary / 'native-window.png'

    def system_events(self, script):
        # Use the runner's system automation client. Do not modify TCC or
        # preemptively reject it based on an installed Python's AX identity.
        result = subprocess.run(['/usr/bin/osascript', '-e', script], capture_output=True, timeout=10)
        if result.returncode:
            raise AppleScriptError(result.returncode, result.stderr)
        return result.stdout.decode('utf-8', errors='replace').strip()

    def windows(self):
        items = self.q.CGWindowListCopyWindowInfo(self.q.kCGWindowListOptionOnScreenOnly, self.q.kCGNullWindowID) or []
        found = []
        for item in items:
            bounds = item.get(self.q.kCGWindowBounds, {})
            if item.get(self.q.kCGWindowOwnerPID) == self.pid and bounds.get('Width', 0) > 300 and bounds.get('Height', 0) > 200:
                found.append(int(item[self.q.kCGWindowNumber]))
        return found

    def native_dialog(self, main):
        other = next((window for window in self.windows() if window != main), None)
        if other is not None:
            return other
        # NSOpenPanel can be a native sheet without a separate app-owned
        # CGWindow. Ask the actual accessibility tree, never infer from pixels.
        # Loop variables are references to list items. Resolve their contents
        # under System Events, then count the returned lists locally; telling
        # the process reference to count its sheets fails with AppleScript -1708.
        script = f'''
tell application "System Events"
    set targetProcesses to get every process whose unix id is {self.pid}
    set sheetCount to 0
    repeat with targetProcess in targetProcesses
        set targetWindows to get every window of (contents of targetProcess)
        repeat with currentWindow in targetWindows
            set windowSheets to get every sheet of (contents of currentWindow)
            set sheetCount to sheetCount + (length of windowSheets)
        end repeat
    end repeat
    return sheetCount
end tell
'''
        # An AX lookup error is not evidence that the sheet has closed. Keep
        # the typed error so wait() can retry both open and close predicates.
        count = self.system_events(script)
        return main if int(count) > 0 else None

    def bounds(self, window):
        items = self.q.CGWindowListCopyWindowInfo(self.q.kCGWindowListOptionIncludingWindow, window) or []
        bounds = next((item[self.q.kCGWindowBounds] for item in items if item.get(self.q.kCGWindowNumber) == window), None)
        if bounds is None:
            raise RuntimeError('Native macOS window no longer exists')
        return bounds

    def screen_bounds(self):
        display = self.q.CGDisplayBounds(self.q.CGMainDisplayID())
        return display.origin.x, display.origin.y, display.size.width, display.size.height

    def prepare(self, window):
        self.focus(window)
        bounds = self.bounds(window)
        left, top, width, height = self.screen_bounds()
        if width < 880 or height < 680:
            raise RuntimeError('macOS runner has no usable interactive desktop')
        if bounds['X'] < left or bounds['Y'] < top or bounds['X'] + bounds['Width'] > left + width or bounds['Y'] + bounds['Height'] > top + height:
            # This runs before any modal exists, so AX window 1 is the main
            # window. Resize/reposition the real window rather than crop it.
            size = f"{{{min(round(bounds['Width']), round(width))}, {min(round(bounds['Height']), round(height) - 30)}}}"
            position = f'{{{round(left)}, {round(top) + 30}}}'
            self.system_events(f'tell application "System Events" to tell (first process whose unix id is {self.pid})\n'
                               f'set size of window 1 to {size}\nset position of window 1 to {position}\nend tell')
            time.sleep(0.2)

    def resize(self, window, width, height):
        self.focus(window)
        frame = self.appkit.NSWindow.frameRectForContentRect_styleMask_(((0., 0.), (float(width), float(height))), 15)
        left, top, screen_width, screen_height = self.screen_bounds()
        if frame.size.width > screen_width or frame.size.height + 30 > screen_height:
            raise RuntimeError('macOS desktop cannot fit the minimum native client viewport')
        self.system_events(f'tell application "System Events" to tell (first process whose unix id is {self.pid})\n'
                           f'set size of window 1 to {{{round(frame.size.width)}, {round(frame.size.height)}}}\n'
                           f'set position of window 1 to {{{round(left)}, {round(top) + 30}}}\nend tell')
        time.sleep(0.2)
        bounds = self.bounds(window)
        if abs(bounds['Width'] - frame.size.width) > 1 or abs(bounds['Height'] - frame.size.height) > 1:
            raise RuntimeError('macOS did not apply the requested compact native viewport')

    def image_scale(self, window, image):
        return image.width / self.bounds(window)['Width']

    def click_capture(self, window, image, x, y):
        self.focus(window)
        bounds = self.bounds(window)
        # Screenshot pixels already include native titlebar/content offsets.
        # Map the observed control directly into WindowServer screen points;
        # do not add a guessed NSWindow titlebar height a second time.
        screen_x = round(bounds['X'] + x * bounds['Width'] / image.width)
        screen_y = round(bounds['Y'] + y * bounds['Height'] / image.height)
        # System Events' accessibility click can succeed without producing an
        # egui pointer event. Post real Quartz move/down/up events instead,
        # from the same system osascript identity used by the runner's native
        # automation permissions. No TCC database is changed.
        script = f'''
ObjC.import('CoreGraphics');
ObjC.import('Foundation');
const point = $.CGPointMake({screen_x}, {screen_y});
const source = $.CGEventSourceCreate($.kCGEventSourceStateHIDSystemState);
function post(kind) {{
    const event = $.CGEventCreateMouseEvent(source, kind, point, $.kCGMouseButtonLeft);
    $.CGEventSetIntegerValueField(event, $.kCGMouseEventClickState, 1);
    $.CGEventPost($.kCGHIDEventTap, event);
    $.NSThread.sleepForTimeInterval(0.08);
}}
post($.kCGEventMouseMoved);
post($.kCGEventLeftMouseDown);
post($.kCGEventLeftMouseUp);
'''
        result = subprocess.run(['/usr/bin/osascript', '-l', 'JavaScript', '-e', script],
                                capture_output=True, timeout=10)
        if result.returncode:
            detail = result.stderr.decode('utf-8', errors='replace')[-1000:].strip()
            raise RuntimeError('macOS native pointer injection failed: ' + detail)
        time.sleep(0.2)

    def focus(self, window):
        application = self.appkit.NSRunningApplication.runningApplicationWithProcessIdentifier_(self.pid)
        if not application or not application.activateWithOptions_(self.appkit.NSApplicationActivateIgnoringOtherApps):
            raise RuntimeError('macOS could not activate the real application window')
        time.sleep(0.15)

    def key(self, name, command=False, window=None):
        if window is not None:
            self.focus(window)
        code = {'down': 125, 'escape': 53, 'e': 14, 'o': 31, 'q': 12}[name]
        self.system_events('tell application "System Events" to key code '
                           + str(code) + (' using {command down}' if command else ''))
        time.sleep(0.2)

    def capture(self, window):
        bounds = self.bounds(window)
        left, top, width, height = self.screen_bounds()
        if bounds['X'] < left or bounds['Y'] < top or bounds['X'] + bounds['Width'] > left + width or bounds['Y'] + bounds['Height'] > top + height:
            raise RuntimeError('Native macOS window exceeds the available desktop; refusing a clipped screenshot')
        # Capture the actual composited desktop region, so an attached native
        # sheet is included even when WindowServer assigns it a separate owner.
        region = ','.join(str(round(bounds[key])) for key in ('X', 'Y', 'Width', 'Height'))
        self.capture_path.unlink(missing_ok=True)
        result = subprocess.run(['/usr/sbin/screencapture', '-x', '-R', region, str(self.capture_path)],
                                capture_output=True, timeout=10)
        if result.returncode or not self.capture_path.is_file():
            raise RuntimeError('macOS real-window screenshot failed; Screen Recording permission or an interactive desktop may be unavailable')
        with Image.open(self.capture_path) as source:
            return source.convert('RGB')


def wait(application, operation, description, timeout=35):
    deadline = time.monotonic() + timeout
    last_stale_error = None
    while time.monotonic() < deadline:
        if application.poll() is not None:
            raise RuntimeError(f'GUI exited with {application.returncode} while waiting for {description}')
        try:
            result = operation()
        except AppleScriptError as error:
            # Only an actual object/index race retries. In particular, a stale
            # object must not look like None to a "dialog is closed" predicate.
            if error.error_code not in (-1728, -1719):
                raise
            last_stale_error = error
        else:
            if result:
                return result
        time.sleep(0.15)
    if last_stale_error is not None:
        raise RuntimeError(f'Timed out waiting for {description}; last AX lookup error: {last_stale_error}') from last_stale_error
    raise RuntimeError('Timed out waiting for ' + description)


def settled(application, desktop, window, path, require_photo=False):
    previous, since = None, time.monotonic()
    geometry_error = None
    def ready():
        nonlocal previous, since, geometry_error
        image = desktop.capture(window)
        colors = image.resize((80, 60)).getcolors(maxcolors=20)
        if min(image.size) < 200 or (colors is not None and len(colors) < 20):
            return False
        if require_photo and image.getcolors(maxcolors=3000) is not None:
            return False
        # Native text carets may blink while the rest of the UI is settled.
        # Permit a tiny changed area, but save the unmodified real capture.
        changed = image.width * image.height
        if previous is not None and previous.size == image.size:
            changed = ImageChops.difference(previous, image).convert('L').point(lambda value: 255 if value > 8 else 0).histogram()[255]
        if changed > image.width * image.height * 0.001:
            previous, since = image, time.monotonic()
            return False
        if time.monotonic() - since < 0.4:
            return False
        if require_photo:
            try:
                photo_geometry((*image.size, image.convert('RGB').tobytes()), desktop.image_scale(window, image))
            except RuntimeError as error:
                if str(error) != geometry_error:
                    geometry_error = str(error)
                    image.save(path.with_name(path.stem + '-incomplete.png'))
                return False
        image.save(path)
        return image
    try:
        return wait(application, ready, 'real rendered ' + path.name)
    except RuntimeError as error:
        if geometry_error:
            raise RuntimeError(f'{error}; last photo measurement: {geometry_error}') from error
        raise


def print_failure_logs(output):
    """Keep the actual native startup error visible even if artifacts fail."""
    for name in ('application-stderr.log', 'application-stdout.log'):
        path = output / name
        try:
            with path.open('rb') as log:
                log.seek(0, os.SEEK_END)
                size = log.tell()
                log.seek(max(0, size - 8192))
                lines = log.read(8192).decode('utf-8', errors='replace').splitlines()
            print(f'--- {name}: last 8 KiB / 60 lines ---', file=sys.stderr)
            print('\n'.join(lines[-60:]).strip() or '(empty)', file=sys.stderr, flush=True)
        except OSError as error:
            print(f'Unable to read {name}: {type(error).__name__}', file=sys.stderr, flush=True)


def compact_selected_tab(image, scale):
    return selected_tab_geometry((*image.size, image.convert('RGB').tobytes()), scale)


def compact_location_point(image, scale):
    return compact_tab_points((*image.size, image.convert('RGB').tobytes()), scale)[2]


def record_photo_times(report, stage, paths):
    # stat only: never read photo contents or restore a baseline while the
    # application or an OS thumbnail service may still be using the files.
    observed = {path.name: snapshot(path) for path in paths}
    report.setdefault('photo_timestamps', {})[stage] = observed
    return observed


def verify_photo_times(report, paths, expected_times):
    observed = record_photo_times(report, 'after_exit', paths)
    differences = {}
    for path in paths:
        changed = {
            field: {'expected': value, 'observed': observed[path.name][field]}
            for field, value in expected_times[path].items()
            if observed[path.name][field] != value
        }
        if changed:
            differences[path.name] = changed
    report['photo_timestamp_differences'] = differences
    if differences:
        raise AssertionError('Browsing changed photo timestamps/identity: '
                             + json.dumps(differences, ensure_ascii=False, sort_keys=True))


def run(args):
    if sys.platform not in ('win32', 'darwin'):
        raise RuntimeError('Use check_gui.py/check_gui_launch.py for real Linux/X11 captures')
    binary = Path(args.binary).resolve()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    report = {'platform': sys.platform, 'real_native_windows': False, 'screenshots': [], 'status': 'failed'}
    application = desktop = main = None
    primary_error = None
    try:
        oracle = PortableOracle()
        with (tempfile.TemporaryDirectory(prefix='photopinfix-native-') as folder,
              tempfile.TemporaryDirectory(prefix='photopinfix-picker-') as picker_folder):
            temporary = Path(folder)
            photos = temporary / '示例 照片'
            picker_directory = Path(picker_folder) / '选择文件夹'
            picker_directory.mkdir()
            executable = temporary / binary.name
            shutil.copy2(binary, executable)
            empty = temporary / 'empty-path'
            empty.mkdir()
            env = os.environ.copy()
            env.update(PATH=str(empty), PHOTOPINFIX_GUI_FOREGROUND='1')
            env.pop('PHOTOPINFIX_GUI_CHILD', None)
            for name in ('PYTHONPATH', 'PYTHONHOME', 'PERL5LIB', 'PERL5OPT', 'PHOTOPINFIX_TEST_EXIFTOOL'):
                env.pop(name, None)
            if args.windows_opengl_dir:
                report['test_opengl'] = windows_test_opengl(args.windows_opengl_dir, temporary, env)
            try:
                with (output / 'application-stdout.log').open('wb') as stdout, (output / 'application-stderr.log').open('wb') as stderr:
                    # Native panels can read photo thumbnails in a separate OS
                    # process and update atime. Exercise that real panel first,
                    # before any protected photo fixtures exist. Application I/O
                    # is then checked in a separate process and temporary root.
                    application = subprocess.Popen([str(executable), '--gui', str(picker_directory)], cwd=temporary, env=env, stdout=stdout, stderr=stderr)
                    desktop = (WindowsDesktop if sys.platform == 'win32' else MacDesktop)(application.pid, temporary)
                    main = wait(application, lambda: next(iter(desktop.windows()), None), 'native picker application window')
                    desktop.prepare(main)
                    settled(application, desktop, main, output / 'picker-initial.png')
                    desktop.key('o', command=True, window=main)
                    native = wait(application, lambda: desktop.native_dialog(main), 'OS-native folder dialog or attached sheet')
                    desktop.focus(native)
                    settled(application, desktop, native, output / 'native-folder-dialog.png')
                    desktop.key('escape', window=native)
                    wait(application, lambda: desktop.native_dialog(main) is None, 'native folder dialog cancellation')
                    settled(application, desktop, main, output / 'picker-closed.png')
                    desktop.key('q', command=True, window=main)
                    assert application.wait(timeout=20) == 0, 'Native picker application exit was unsuccessful'
                    report.update(native_picker_cancelled=True, picker_uses_separate_directory=True)
                    desktop = main = None

                    photos.mkdir()
                    paths, original, expected_times = fixtures(photos, oracle, documentation=True)
                    report['photo_timestamps'] = {
                        'expected': {path.name: expected_times[path] for path in paths},
                    }
                    application = subprocess.Popen([str(executable), '--gui', str(photos)], cwd=temporary, env=env, stdout=stdout, stderr=stderr)
                    desktop = (WindowsDesktop if sys.platform == 'win32' else MacDesktop)(application.pid, temporary)
                    main = wait(application, lambda: next(iter(desktop.windows()), None), 'native application window')
                    desktop.prepare(main)
                    if sys.platform == 'win32':
                        report['window_geometry'] = desktop.preparation
                    settled(application, desktop, main, output / 'initial.png')
                    record_photo_times(report, 'after_initial_frame', paths)
                    desktop.key('down', window=main)
                    overview = settled(application, desktop, main, output / 'overview.png', require_photo=True)
                    record_photo_times(report, 'after_photo_preview', paths)
                    report['photo_geometry'] = {'overview': photo_geometry(
                        (*overview.size, overview.convert('RGB').tobytes()), desktop.image_scale(main, overview))}
                    desktop.key('e', window=main)
                    editor = settled(application, desktop, main, output / 'gps-editor.png')
                    difference = ImageChops.difference(overview, editor).convert('L').point(lambda value: 255 if value > 12 else 0)
                    assert difference.histogram()[255] > overview.width * overview.height * 0.04, 'Editor shortcut did not visibly open its form'
                    desktop.key('escape', window=main)
                    settled(application, desktop, main, output / 'editor-closed.png', require_photo=True)
                    record_photo_times(report, 'after_editor_cancellation', paths)
                    desktop.resize(main, 880, 640)
                    compact = settled(application, desktop, main, output / 'compact-photo.png', require_photo=True)
                    scale = desktop.image_scale(main, compact)
                    report['photo_geometry']['compact'] = photo_geometry((*compact.size, compact.convert('RGB').tobytes()), scale)
                    point = compact_location_point(compact, scale)
                    report['compact_click'] = {'image_pixels': list(point), 'pixels_per_point': scale, 'image_size': list(compact.size)}
                    desktop.click_capture(main, compact, *point)
                    location = settled(application, desktop, main, output / 'compact-location.png')
                    x1, y1, x2, y2 = compact_selected_tab(location, scale)
                    assert x1 <= point[0] <= x2 and y1 <= point[1] <= y2, 'The location tab did not become visibly selected'
                    difference = ImageChops.difference(compact, location).convert('L').point(lambda value: 255 if value > 12 else 0)
                    assert difference.histogram()[255] > compact.width * compact.height * 0.04, 'Compact location tab did not visibly replace the photo view'
                    record_photo_times(report, 'after_compact_navigation', paths)
                    desktop.key('q', command=True, window=main)
                    assert application.wait(timeout=20) == 0, 'GUI exit was unsuccessful'
                    verify_photo_times(report, paths, expected_times)
                    for path in paths:
                        assert preserved_read(path, path.read_bytes) == original[path], f'Browsing changed photo data: {path.name}'
                        assert snapshot(path) == expected_times[path], f'Photo verification changed timestamps/identity: {path.name}'
                    report.update(real_native_windows=True, status='success', native_picker_cancelled=True,
                                  empty_path=True, app_io_times_preserved=True, untouched_photos=True,
                                  screenshots=['overview.png', 'gps-editor.png', 'native-folder-dialog.png', 'compact-photo.png', 'compact-location.png'])
            except BaseException as error:
                primary_error = error
                # Capture before terminating the real window. Diagnostics
                # must not replace the original automation/acceptance error.
                if desktop is not None and main is not None and application.poll() is None:
                    try:
                        desktop.capture(main).save(output / 'failure-main-window.png')
                        report['failure_screenshot'] = 'failure-main-window.png'
                    except Exception as screenshot_error:
                        report['failure_screenshot_error'] = str(screenshot_error)[-2000:]
                raise
            finally:
                # Windows locks the copied executable and Mesa DLLs while the
                # process runs. Release them before TemporaryDirectory exits.
                if application and application.poll() is None:
                    application.terminate()
                    try:
                        application.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        application.kill()
                        application.wait(timeout=5)
    except Exception as error:
        report['status'] = 'failed'
        if primary_error is not None and error is not primary_error:
            report['cleanup_error'] = str(error)
            report['error'] = str(primary_error)
            print_failure_logs(output)
            raise primary_error from error
        report['error'] = str(error)
        print_failure_logs(output)
        raise
    finally:
        (output / 'capture-report.json').write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')
        # Redirected Windows stdout may use cp1252; keep the console JSON
        # ASCII-safe while preserving readable UTF-8 in the report artifact.
        print(json.dumps(report, indent=2, ensure_ascii=True), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary')
    parser.add_argument('--output', default=str(ROOT / 'target/native-screenshots'))
    parser.add_argument('--windows-opengl-dir', help='Explicit CI-only directory of manifest-verified Windows software OpenGL DLLs')
    run(parser.parse_args())
