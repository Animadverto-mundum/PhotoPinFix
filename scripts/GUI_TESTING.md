# Native GUI integration test

These are Linux development tools, not application dependencies. The shipped
executable does not run Python, Perl, Xvfb or ExifTool.

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
usable after the terminal closes. The test clicks the welcome action, types a
folder path containing Chinese characters and spaces, confirms the built-in
folder picker with the mouse, and waits for actual photo pixels. It then closes
the window and verifies an observed successful exit plus unchanged photo bytes,
file times and identity. Its `gui-launch-*.png`, launcher/Xvfb logs and
`gui-launch-report.json` describe this separate acceptance result. No desktop
file chooser program, clipboard utility or shell command is available on the
application's empty PATH.

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

A successful test establishes this Linux/X11 configuration; native Windows and
macOS detached-launch and UI behavior still require acceptance on those
platforms. The PTY/session assertions are Linux-specific and are not evidence
that Windows process flags or macOS terminal closing have been exercised.
Linux CI runs both scripts and retains `gui-launch-*` diagnostics if the launch
acceptance fails; existing desktop/TUI screenshot artifacts remain separate.
Test screenshots
and logs are local development artifacts and should not be committed or shared
without checking that displayed paths contain no personal information.
