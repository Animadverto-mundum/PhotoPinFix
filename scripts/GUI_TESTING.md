# Native GUI integration test

These are development tools, not application dependencies. The shipped
executable does not run Python, Perl, Xvfb or ExifTool. Linux tests use a private
virtual desktop; Windows and macOS captures use the runner's actual desktop.

Prepare local Xvfb without sudo using `bash scripts/prepare_gui_test.sh`. The
script also obtains the embedded font and its license. It uses the distribution
package manager's download operation and extracts into `tools/gui-test/root`.
For a stale Ubuntu 20.04 amd64 package index, it can use the exact Ubuntu archive
package pinned by SHA256 in the script. Other distributions need their own
working package index or a system Xvfb. The host must have X11, XTest and Mesa
software rendering libraries. No existing desktop session is controlled.
Development installations may live outside the checkout: set
`PHOTOPINFIX_TEST_XVFB` to the Xvfb executable and
`PHOTOPINFIX_TEST_EXIFTOOL` to the ExifTool script. These overrides are used only
by the test harness, are removed from the application's environment, and are
not recorded in its report. Perl must be on the test harness's PATH.
The test needs permission to create its own local Unix socket. Its preflight
reports a restricted sandbox immediately; run the test in a local development
environment that permits its private display socket.

After building the executable, run:

```sh
python3 scripts/check_gui.py target/debug/photopinfix
# The same test can validate the standalone release executable:
python3 scripts/check_gui.py target/release/photopinfix --artifacts target/gui-release-check
# Verify the public --gui launcher and welcome-folder flow separately:
python3 scripts/check_gui_launch.py target/release/photopinfix --artifacts target/gui-launch-check
```

The test uses Python's standard library and ctypes bindings to X11/XTest, starts
its own virtual display, and copies the executable into a temporary directory
outside the checkout. It launches the application with an empty PATH and invalid
external-tool overrides. Disposable Chinese-named HEIC and PNG fixtures exercise
preview, coordinate editing, decimal/DMS and signed/hemisphere conversion,
current-photo confirmation and cancellation, batch writes, sessions, settings,
and the minimum 880 × 640 viewport. Screenshots, application logs and a JSON
report are saved under `target/test-artifacts` by default.

`check_gui.py` sets the internal `PHOTOPINFIX_GUI_FOREGROUND=1` switch so the
harness can observe the actual window process and its exit status. Normal
`photopinfix --gui` usage does not need this switch: its launcher returns after
the detached window reports that it is ready.

`check_gui_launch.py` exercises that normal launch with no directory argument.
It creates a real controlling PTY, requires the launcher to exit successfully,
then closes the PTY before interacting with the detached window. It checks that
the window has its own Unix session and no controlling terminal, displays the
welcome page even when its working directory contains an image, and remains
usable after the terminal closes. The test clicks the welcome action, cancels
the desktop's real GTK folder selector, reopens it, then selects a real folder
whose name contains Chinese characters and spaces. GTK's native list search
uses its ASCII prefix; the returned URI retains the complete Unicode path.
The test confirms the selection and waits for actual photo pixels. It then closes
the window and verifies an observed successful exit plus unchanged photo bytes,
file times and identity. Its `gui-launch-*.png`, launcher/Xvfb logs and
`gui-launch-report.json` describe this separate acceptance result. No desktop
file chooser program, clipboard utility or shell command is available on the
application's empty PATH. The Linux app calls XDG Desktop Portal over D-Bus;
the test starts real `xdg-desktop-portal` and `xdg-desktop-portal-gtk` services
on its private bus. The services must support FileChooser API v3 or newer
(portal 1.8 or later); older backends only support files. Install those packages
plus `dbus` and `libglib2.0-bin` for
development, or point `PHOTOPINFIX_TEST_PORTAL` and
`PHOTOPINFIX_TEST_PORTAL_GTK` at extracted executables. The test also recognizes
`tools/gui-test/portal-root/usr/libexec`. It uses a disposable copy of the
bundled Chinese font for GTK. A missing portal or missing native dialog fails
the acceptance test; no fake response or custom picker is substituted.

Writing a photo keeps the other reviewed candidates unchanged, matching the
TUI; each confirmed batch uses the exact candidates shown in its confirmation
dialog. This test writes the manual 10:00:30 photo first, then expects the
10:00:45 automatic candidate from the original 10:00/10:01 anchors (longitude
-70.2). It does not request a recomputation using the newly written photo.

The test process uses the developer copy `tools/exiftool/exiftool` and Perl to
prepare fixtures and independently check final GPS and provenance. Those tools
are not available to the application. It also checks byte-identical untouched
photos and backups, and exact access/modification/birth times plus inode identity
when the filesystem exposes birth time. Metadata is inspected only after the
application has gracefully exited and joined its I/O worker. This avoids races
between the independent oracle and application access-time guards.

A successful launch test establishes this Linux/X11 configuration. The
PTY/session assertions are Linux-specific and do not establish Windows process
flag behavior or macOS behavior after a terminal closes. Linux CI runs both
scripts and retains `gui-launch-*` diagnostics if acceptance fails.

Windows and macOS CI additionally run:

```sh
python scripts/capture_native_gui.py path/to/photopinfix --output target/native-screenshots
```

On Windows use the `.exe` path. The development harness needs Pillow and the
independent Perl/ExifTool oracle. macOS additionally uses
`pyobjc-framework-Quartz` and `pyobjc-framework-Cocoa`; it sends keys through
the system `osascript`/System Events and posts actual Quartz mouse events from
`osascript`. It captures the actual window's screen
region with `screencapture`. It never edits privacy-permission databases. The
runner must provide an interactive desktop and permit native automation and
screen capture. Missing permissions cause an explicit failure report, not a
synthetic image or a successful skip. Windows uses native Win32 window/input
APIs and real desktop captures.

The standard Windows runner's built-in OpenGL implementation is too old for
`egui_glow`. Windows CI therefore prepares a **test-only** LLVMpipe driver:

```powershell
./scripts/prepare_windows_gui_test.ps1
python scripts/capture_native_gui.py path/to/photopinfix.exe --output target/native-screenshots --windows-opengl-dir tools/gui-test/mesa-x64
```

The preparation script pins the published
[Mesa 25.1.9 MSVC archive](https://github.com/pal1000/mesa-dist-win/releases/tag/25.1.9)
and verifies its SHA256 against the value published with that release before
extracting the required x64 DLLs. It checks their PE architecture and import
dependencies without loading them. The fixed DLL set is `opengl32.dll` and
`libgallium_wgl.dll`; the latter's imports are Windows system libraries, so no
separate LLVM, OpenCL or Vulkan runtime is needed. Both DLL hashes are also
pinned in the preparation script. The capture harness verifies the generated
manifest and each DLL again, then places those DLLs beside its disposable copy
of the executable and selects `llvmpipe`; the application's PATH remains empty.
`capture-report.json` identifies this software-rendering test environment. The
driver is never installed system-wide, never changes the product's renderer,
and is never copied into the app or corresponding-source packages. This follows
[Mesa's per-application Windows deployment](https://docs.mesa3d.org/drivers/llvmpipe.html#windows).
The screenshot proves rendering on this CI software driver; it does not prove
compatibility with every user's Windows graphics driver.

The capture sequence records a public landscape photo, the coordinate editor,
and the OS-native folder dialog (COM on Windows, NSOpenPanel on macOS), then
cancels the picker, resizes the real client viewport to 880 × 640, and captures
its photo and location tabs. It verifies unchanged photo bytes and times. It recognizes
macOS attached sheets through the actual accessibility tree as well as separate
windows. The copied application runs outside the checkout with an empty PATH.
These captures exercise real rendering and picker cancellation; they do not
replace platform-specific detached-launch or confirmed-write acceptance.

Both capture harnesses locate compact tabs in the unmodified screenshot and
verify that a real click selects the intended view. The shared `gui_visual.py`
also measures the preview canvas and actual photo pixels: an opened window
containing only a tiny thumbnail or a loading label fails visual acceptance.
Reports record those dimensions in logical points. These geometry checks
support, and do not replace, visual review of typography, spacing and clipping.

All three CI jobs upload `photopinfix-<target>-gui-screenshots`; the original
Linux desktop/TUI screenshot artifact remains available. `capture-report.json`
records success or the reason capture failed. Screenshots and logs should be
reviewed for personal paths before sharing; the fixtures use disposable sample
folders rather than a user's photo library.
