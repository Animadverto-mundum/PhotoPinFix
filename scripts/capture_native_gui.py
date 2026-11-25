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
        self.u.GetSystemMetrics.argtypes = [C.c_int]
        try:
            self.u.SetProcessDPIAware()
        except AttributeError:
            pass
        if self.u.GetSystemMetrics(0) < 880 or self.u.GetSystemMetrics(1) < 640:
            raise RuntimeError('Windows runner has no usable interactive desktop')

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
        # Hosted Windows desktops can be smaller than the app's preferred
        # window. Keep its real rendered client area inside the display.
        rect = self.W.RECT()
        if self.u.GetWindowRect(window, C.byref(rect)):
            width, height = self.u.GetSystemMetrics(0), self.u.GetSystemMetrics(1)
            if rect.left < 0 or rect.top < 0 or rect.right > width or rect.bottom > height:
                self.u.MoveWindow(window, 0, 0, min(rect.right - rect.left, width), min(rect.bottom - rect.top, height), True)
        self.u.SetForegroundWindow(window)
        time.sleep(0.15)
        if int(self.u.GetForegroundWindow() or 0) != window:
            raise RuntimeError('Windows refused foreground control; real UI input cannot be verified')

    def native_dialog(self, main):
        return next((window for window in self.windows() if window != main), None)

    def prepare(self, window):
        self.focus(window)

    def resize(self, window, width, height):
        outer, inner = self.W.RECT(), self.W.RECT()
        if not self.u.GetWindowRect(window, C.byref(outer)) or not self.u.GetClientRect(window, C.byref(inner)):
            raise RuntimeError('Cannot resize a missing Windows window')
        scale = self.u.GetDpiForWindow(window) / 96.0
        desired = (round(width * scale), round(height * scale))
        total_width = desired[0] + outer.right - outer.left - inner.right
        total_height = desired[1] + outer.bottom - outer.top - inner.bottom
        if total_width > self.u.GetSystemMetrics(0) or total_height > self.u.GetSystemMetrics(1):
            raise RuntimeError('Windows desktop cannot fit the minimum client viewport at its current DPI')
        if not self.u.MoveWindow(window, 0, 0, total_width, total_height, True):
            raise RuntimeError('Windows refused to resize the real window')
        time.sleep(0.2)
        self.u.GetClientRect(window, C.byref(inner))
        if (inner.right, inner.bottom) != desired:
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
        if rect.left < 0 or rect.top < 0 or rect.right > self.u.GetSystemMetrics(0) or rect.bottom > self.u.GetSystemMetrics(1):
            raise RuntimeError('Native Windows window exceeds the available desktop; refusing a clipped or padded screenshot')
        return ImageGrab.grab(bbox=(rect.left, rect.top, rect.right, rect.bottom), all_screens=True).convert('RGB')


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
            raise RuntimeError('macOS System Events refused native UI automation; the runner needs its supported Accessibility/Automation permissions')
        return result.stdout.decode().strip()

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
        count = self.system_events('tell application "System Events" to tell '
                                   f'(first process whose unix id is {self.pid}) '
                                   'to return count of sheets of window 1')
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
    while time.monotonic() < deadline:
        if application.poll() is not None:
            raise RuntimeError(f'GUI exited with {application.returncode} while waiting for {description}')
        result = operation()
        if result:
            return result
        time.sleep(0.15)
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


def run(args):
    if sys.platform not in ('win32', 'darwin'):
        raise RuntimeError('Use check_gui.py/check_gui_launch.py for real Linux/X11 captures')
    binary = Path(args.binary).resolve()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    report = {'platform': sys.platform, 'real_native_windows': False, 'screenshots': [], 'status': 'failed'}
    application = desktop = None
    primary_error = None
    try:
        oracle = PortableOracle()
        with tempfile.TemporaryDirectory(prefix='photopinfix-native-') as folder:
            temporary = Path(folder)
            photos = temporary / '示例 照片'
            photos.mkdir()
            paths, original, expected_times = fixtures(photos, oracle, documentation=True)
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
                    application = subprocess.Popen([str(executable), '--gui', str(photos)], cwd=temporary, env=env, stdout=stdout, stderr=stderr)
                    desktop = (WindowsDesktop if sys.platform == 'win32' else MacDesktop)(application.pid, temporary)
                    main = wait(application, lambda: next(iter(desktop.windows()), None), 'native application window')
                    desktop.prepare(main)
                    settled(application, desktop, main, output / 'initial.png')
                    desktop.key('down', window=main)
                    overview = settled(application, desktop, main, output / 'overview.png', require_photo=True)
                    report['photo_geometry'] = {'overview': photo_geometry(
                        (*overview.size, overview.convert('RGB').tobytes()), desktop.image_scale(main, overview))}
                    desktop.key('e', window=main)
                    editor = settled(application, desktop, main, output / 'gps-editor.png')
                    difference = ImageChops.difference(overview, editor).convert('L').point(lambda value: 255 if value > 12 else 0)
                    assert difference.histogram()[255] > overview.width * overview.height * 0.04, 'Editor shortcut did not visibly open its form'
                    desktop.key('escape', window=main)
                    settled(application, desktop, main, output / 'editor-closed.png', require_photo=True)
                    desktop.key('o', command=True, window=main)
                    native = wait(application, lambda: desktop.native_dialog(main), 'OS-native folder dialog or attached sheet')
                    desktop.focus(native)
                    settled(application, desktop, native, output / 'native-folder-dialog.png')
                    desktop.key('escape', window=native)
                    wait(application, lambda: desktop.native_dialog(main) is None, 'native folder dialog cancellation')
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
                    desktop.key('q', command=True, window=main)
                    assert application.wait(timeout=20) == 0, 'GUI exit was unsuccessful'
                    for path in paths:
                        assert snapshot(path) == expected_times[path], 'Browsing changed photo timestamps/identity'
                        assert preserved_read(path, path.read_bytes) == original[path], 'Browsing changed photo data'
                    report.update(real_native_windows=True, status='success', native_picker_cancelled=True,
                                  empty_path=True, untouched_photos=True,
                                  screenshots=['overview.png', 'gps-editor.png', 'native-folder-dialog.png', 'compact-photo.png', 'compact-location.png'])
            except BaseException as error:
                primary_error = error
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
        print(json.dumps(report, indent=2, ensure_ascii=False), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary')
    parser.add_argument('--output', default=str(ROOT / 'target/native-screenshots'))
    parser.add_argument('--windows-opengl-dir', help='Explicit CI-only directory of manifest-verified Windows software OpenGL DLLs')
    run(parser.parse_args())
