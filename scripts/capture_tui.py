#!/usr/bin/env python3
"""Capture the real PhotoPinFix TUI in an isolated X11 xterm window.

Developer-only Linux tooling: Xvfb, xterm, util-linux script, pyte, and the
independent ExifTool fixture oracle. The application runs with an empty PATH.
The PNG is an unmodified X11 window capture, not a rendering of terminal cells.
"""

import argparse
import codecs
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
from xml.sax.saxutils import escape

from check_gui import Oracle, ROOT, X11, fixtures, png, preserved_read, snapshot

sys.path.insert(0, str(ROOT / "tools/test-deps"))
import pyte


class CaptureScreen(pyte.Screen):
    """Observe ratatui's end-of-frame cursor-hide command, including split CSI."""

    on_frame = None

    def reset_mode(self, *modes, **kwargs):
        super().reset_mode(*modes, **kwargs)
        if kwargs.get("private") and 25 in modes and self.on_frame:
            self.on_frame()


class Transcript:
    """Observe output for synchronization; xterm alone renders the screenshot."""

    def __init__(self, path, columns, rows):
        self.path = path
        self.position = 0
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.pending = ""
        self.screen = CaptureScreen(columns, rows)
        self.stream = pyte.Stream(self.screen)
        self.frame = 0
        self.images = {}
        self.completed_selection = ""
        self.completed_photo_text = ""
        self.screen.on_frame = self.finish_frame

    def lines(self):
        return ["".join(self.screen.buffer[y][x].data for x in range(self.screen.columns))
                for y in range(self.screen.lines)]

    def panes(self):
        lines = self.lines()
        photo = next((y for y, line in enumerate(lines) if "照片预览" in line), None)
        map_row = next((y for y, line in enumerate(lines) if "地图 ·" in line), None)
        if photo is None or map_row is None:
            return {}
        # Match the fixed screenshot layout: two equal columns, bordered photo
        # and map, with three map-information rows above the two-line footer.
        left, right = self.screen.columns // 2 + 1, self.screen.columns - 1
        return {"photo": (left, photo + 1, right, map_row - 1),
                "map": (left, map_row + 1, right, self.screen.lines - 6)}

    def selection(self):
        return next((line for line in self.lines() if "›" in line), "")

    def cells(self, bounds):
        left, top, right, bottom = bounds
        return [self.screen.buffer[y][x] for y in range(top, bottom) for x in range(left, right)]

    def finish_frame(self):
        self.frame += 1
        self.completed_selection = self.selection()
        panes = self.panes()
        self.completed_photo_text = ("".join(c.data for c in self.cells(panes["photo"]))
                                     .strip(" ▀▄█") if "photo" in panes else "")
        # Halfblocks have no DCS. A changed, colored image region in a completed
        # frame is positive evidence; plain text and an unchanged old image are
        # never counted as the newly selected photo/map.
        for pane, bounds in panes.items():
            cells = self.cells(bounds)
            blocks = [cell for cell in cells if cell.data in ("▀", "▄", "█")]
            if len(blocks) < 40 or len({(cell.fg, cell.bg) for cell in blocks}) < 3:
                continue
            digest = hashlib.sha256(repr([(c.data, c.fg, c.bg) for c in cells]).encode()).hexdigest()
            key = ("halfblocks", pane)
            if self.images.get(key, {}).get("digest") != digest:
                self.images[key] = {"frame": self.frame, "selection": self.selection(), "digest": digest}

    def record_sixel(self, payload):
        header = re.match(r'\x1bP[0-9;?]*q"\d+;\d+;(\d+);(\d+)', payload)
        if not header or min(map(int, header.groups())) < 16:
            return
        # Require real raster data and a palette, not just a DCS envelope.
        raster = re.sub(r"#\d+(?:;\d+)*", "", payload[header.end():-2])
        if len(re.findall(r"#\d+;2;\d+;\d+;\d+", payload)) < 2 or not re.search(r"[@-~]", raster):
            return
        x, y = self.screen.cursor.x, self.screen.cursor.y
        for pane, (left, top, right, bottom) in self.panes().items():
            if left <= x < right and top <= y < bottom:
                digest = hashlib.sha256(payload.encode()).hexdigest()
                key = ("sixel", pane)
                if self.images.get(key, {}).get("digest") != digest:
                    self.images[key] = {"frame": self.frame + 1, "selection": self.selection(),
                                        "digest": digest, "pixels": list(map(int, header.groups()))}

    def read(self):
        if self.path.exists():
            with self.path.open("rb") as log:
                log.seek(self.position)
                data = log.read()
                self.position += len(data)
            self.pending += self.decoder.decode(data)
        # pyte does not render sixel. Remove complete DCS image payloads only
        # from this observer; the original bytes reach xterm and its log intact.
        while self.pending:
            start = self.pending.find("\x1bP")
            if start < 0:
                keep = 1 if self.pending.endswith("\x1b") else 0
                self.stream.feed(self.pending[:-keep] if keep else self.pending)
                self.pending = self.pending[-keep:] if keep else ""
                break
            self.stream.feed(self.pending[:start])
            self.pending = self.pending[start:]
            end = self.pending.find("\x1b\\", 2)
            if end < 0:
                break
            self.record_sixel(self.pending[:end + 2])
            self.pending = self.pending[end + 2:]
        return "\n".join(self.lines())

    def contains(self, text):
        return "".join(text.split()) in "".join(self.read().split())

    def selected(self, name):
        return any("›" in line and name in line for line in self.read().splitlines())

    def mark_images(self):
        self.read()
        return self.frame

    def images_ready(self, name, graphics, after):
        self.read()
        if name not in self.completed_selection or name not in self.selection() or self.frame <= after:
            return False
        panes = self.panes()
        if "photo" not in panes:
            return False
        text = self.completed_photo_text
        if text:
            if "正在读取图片" in text:
                return False
            raise RuntimeError(f"Photo preview failed instead of rendering: {text[:400]}")
        return all((event := self.images.get((graphics, pane)))
                   and after < event["frame"] <= self.frame and name in event["selection"]
                   for pane in ("photo", "map"))


def executable(name, override=None):
    value = override or shutil.which(name)
    if not value or not Path(value).is_file():
        raise RuntimeError(f"Developer screenshot tool is missing: {name}")
    return str(Path(value).resolve())


def fixture_images_visible(transcript, frame):
    """Require the real window to show the forest and green/yellow map markers.

    The transcript can reach disk before xterm has painted it. Inspect the
    actual X11 pixels as well: neither blank panels nor preview-error text can
    supply the fixture's forest colors and both map-marker colors.
    """
    width, height, pixels = frame
    counts = {}
    # xterm is launched with a ten-pixel inner border and a fixed cell grid.
    cell_width = (width - 20) / transcript.screen.columns
    cell_height = (height - 20) / transcript.screen.lines
    for pane, (left, top, right, bottom) in transcript.panes().items():
        greens = yellows = total = 0
        for y in range(round(10 + top * cell_height), round(10 + bottom * cell_height)):
            for x in range(round(10 + left * cell_width), round(10 + right * cell_width)):
                offset = (y * width + x) * 3
                r, g, b = pixels[offset:offset + 3]
                greens += g > r + 12 and g > b + 12 and g > 40
                yellows += r > 160 and g > 130 and b < 130
                total += 1
        counts[pane] = greens, yellows, total
    return ("photo" in counts and "map" in counts
            and counts["photo"][0] > counts["photo"][2] // 50
            and counts["map"][0] > 10 and counts["map"][1] > 30)


def run(args):
    binary = Path(args.binary).resolve()
    if not binary.is_file():
        raise RuntimeError(f"Application executable does not exist: {binary}")
    output = Path(args.output).resolve()
    artifacts = Path(args.artifacts).resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    local_xvfb = ROOT / "tools/gui-test/root/usr/bin/Xvfb"
    xvfb = executable("Xvfb", os.environ.get("PHOTOPINFIX_TEST_XVFB") or
                      (str(local_xvfb) if local_xvfb.is_file() else None))
    xterm = executable("xterm", os.environ.get("PHOTOPINFIX_TEST_XTERM"))
    recorder = executable("script")
    env_program = executable("env")
    oracle = Oracle()
    xserver = terminal = x = None
    logs = []
    columns, rows = 146, 44

    with tempfile.TemporaryDirectory(prefix="photopinfix-terminal-", dir="/tmp") as temporary:
        temp = Path(temporary)
        with socket.socket(socket.AF_UNIX) as probe:
            probe.bind(str(temp / "socket-check"))
        (temp / "socket-check").unlink()
        photos = temp / "示例照片"
        photos.mkdir()
        paths, originals, times = fixtures(photos, oracle, documentation=True)
        runtime = temp / "standalone"
        runtime.mkdir()
        app = runtime / "photopinfix"
        shutil.copyfile(binary, app)
        app.chmod(0o755)
        empty_path = temp / "empty-path"
        empty_path.mkdir()
        cache = temp / "font-cache"
        cache.mkdir()
        fonts = temp / "fonts"
        fonts.mkdir()
        source_font = ROOT / "assets/fonts/NotoSansSC-Regular.otf"
        preserved_read(source_font, lambda: shutil.copyfile(source_font, fonts / source_font.name))
        fontconfig = temp / "fonts.conf"
        fontconfig.write_text(
            '<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">'
            '<fontconfig><include ignore_missing="yes">/etc/fonts/fonts.conf</include>'
            # Fontconfig may create .uuid beside scanned fonts as well as its
            # cache. Scan a disposable copy, never the repository font folder.
            f'<dir>{escape(str(fonts))}</dir>'
            f'<cachedir>{escape(str(cache))}</cachedir></fontconfig>\n'
        )
        transcript_path = artifacts / "tui-window.ansi"
        # A repeated capture must not satisfy readiness from an older run while
        # xterm is still starting the new application.
        transcript_path.write_bytes(b"")
        captured = artifacts / "tui-window.png"
        transcript = Transcript(transcript_path, columns, rows)
        display_read, display_write = os.pipe()
        logs = [open(artifacts / name, "wb") for name in ("tui-xvfb.log", "tui-xterm.log")]
        try:
            xserver = subprocess.Popen(
                [xvfb, "-displayfd", str(display_write), "-screen", "0", "1920x1200x24",
                 "-nolisten", "tcp", "-ac", "-noreset"],
                pass_fds=(display_write,), stdout=logs[0], stderr=logs[0],
            )
            os.close(display_write)
            if not select.select([display_read], [], [], 15)[0]:
                raise RuntimeError("Private Xvfb did not become ready")
            display_number = os.read(display_read, 128).decode().strip()
            os.close(display_read)
            if not display_number.isdecimal():
                raise RuntimeError("Xvfb did not allocate a private display")
            display = ":" + display_number
            x = X11(display)
            environment = os.environ.copy()
            for name in ("NO_COLOR", "PYTHONPATH", "PYTHONHOME", "PERL5LIB", "PERL5OPT",
                         "VIRTUAL_ENV", "CONDA_PREFIX", "PHOTOPINFIX_TEST_EXIFTOOL",
                         "PHOTOPINFIX_TEST_XVFB", "PHOTOPINFIX_TEST_XTERM"):
                environment.pop(name, None)
            environment.update(DISPLAY=display, FONTCONFIG_FILE=str(fontconfig),
                               XDG_CACHE_HOME=str(cache), SHELL="/bin/sh", LANG="C.UTF-8")
            command = shlex.join([
                env_program, f"PATH={empty_path}", "COLORTERM=truecolor",
                "PHOTOPINFIX_EXIFTOOL=/nonexistent/exiftool", "PHOTOPINFIX_PYTHON=/nonexistent/python",
                str(app), "--graphics", args.graphics, str(photos),
            ])
            terminal = subprocess.Popen(
                [xterm, "-T", "PhotoPinFix TUI", "-geometry", f"{columns}x{rows}",
                 "-fa", "DejaVu Sans Mono", "-fd", "Noto Sans SC", "-fs", "11",
                 "-bg", "#101820", "-fg", "#d6dee8", "-b", "10", "+sb", "-u8",
                 "-ti", "vt340", "-xrm", "XTerm*cursorBlink: false",
                 "-xrm", "XTerm*numColorRegisters: 256",
                 "-e", recorder, "-q", "-e", "-f", "-c", command, str(transcript_path)],
                cwd=runtime, env=environment, stdout=logs[1], stderr=logs[1],
            )
            x.process = terminal
            x.wait(x.find_window, "real xterm window appears")
            x.wait(lambda: transcript.contains("找到 5 张照片"), "TUI metadata scan completes")
            x.key("a")
            x.wait(lambda: transcript.contains("绿2 黄2 红1"), "all three GPS states are visible")
            image_marker = transcript.mark_images()
            x.key("j")
            x.wait(lambda: transcript.selected(paths[1].name), "landscape photo is selected")
            x.wait(lambda: transcript.images_ready(paths[1].name, args.graphics, image_marker),
                   "new landscape and selected-point map images finish rendering")
            x.wait(lambda: fixture_images_visible(transcript, x.frame()),
                   "xterm paints the landscape and both GPS marker colors")
            width, height, pixels = x.settled(captured)
            if not transcript.images_ready(paths[1].name, args.graphics, image_marker):
                raise RuntimeError("Photo/map readiness changed while capturing the terminal")
            if not fixture_images_visible(transcript, (width, height, pixels)):
                raise RuntimeError("Photo/map image pixels disappeared while capturing the terminal")
            # A TUI can exit before XTest's key-delivery flush returns. Once q
            # is intentional, inspect its result instead of treating it as an
            # unexpected early exit inside the X11 event helper.
            x.process = None
            x.key("q")
            result = terminal.wait(timeout=30)
            if result != 0:
                raise RuntimeError(f"Terminal/application exited with {result}")
            for path in paths:
                assert snapshot(path) == times[path], f"File times changed: {path.name}"
                assert preserved_read(path, path.read_bytes) == originals[path], f"Photo changed: {path.name}"
                assert snapshot(path) == times[path]
            assert not list(photos.glob("*.photopinfix-original"))
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(captured, output)
            report = {
                "application": binary.name, "terminal": "xterm", "graphics": args.graphics,
                "real_window_capture": True, "image_modified": False,
                "application_path_empty": True, "all_photo_bytes_and_times_preserved": True,
                "selected_photo_and_map_images_confirmed": True,
                "width": width, "height": height,
                "png_sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
            }
            (artifacts / "tui-window-report.json").write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2), flush=True)
        except Exception:
            if x and x.window and terminal and terminal.poll() is None:
                try:
                    png(artifacts / "tui-window-failure.png", *x.frame())
                except Exception:
                    pass
            for log in logs:
                log.flush()
                try:
                    text = Path(log.name).read_bytes()[-8192:].decode(errors="replace")
                    print(f"--- {Path(log.name).name} ---\n{text}", file=sys.stderr, flush=True)
                except OSError:
                    pass
            print(transcript.read(), file=sys.stderr, flush=True)
            raise
        finally:
            if terminal and terminal.poll() is None:
                if x and x.window:
                    try:
                        x.key("Escape")
                        x.key("q")
                    except Exception:
                        pass
                try:
                    terminal.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    terminal.terminate()
                    terminal.wait(timeout=5)
            if x:
                x.close()
            if xserver:
                xserver.terminate()
                xserver.wait(timeout=5)
            for log in logs:
                log.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", default=str(ROOT / "bin/photopinfix"))
    parser.add_argument("--output", default=str(ROOT / "docs/images/tui.png"))
    parser.add_argument("--artifacts", default=str(ROOT / "target/test-artifacts"))
    parser.add_argument("--graphics", choices=("sixel", "halfblocks"), default="sixel")
    run(parser.parse_args())
