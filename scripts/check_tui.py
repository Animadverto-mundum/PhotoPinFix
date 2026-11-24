#!/usr/bin/env python3
"""Linux/macOS PTY integration check. Uses disposable copies, never writes demo photos.

Requires pexpect and pyte. Run after cargo build; optional first argument is binary.
Artifacts are terminal transcripts and screen dumps under target/test-artifacts/.
"""

from pathlib import Path
import json
import os
import select
import shutil
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools/test-deps"))
import pexpect
import pyte

ARTIFACTS = ROOT / "target/test-artifacts"
ARTIFACTS.mkdir(parents=True, exist_ok=True)
BINARY = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / "target/debug/photopinfix"
STANDALONE = os.environ.get("PHOTOPINFIX_TEST_STANDALONE") == "1"


def times(path):
    st = path.stat()
    return st.st_atime_ns, st.st_mtime_ns, st.st_ino


class Tui:
    def __init__(self, directory, name, columns=140, rows=44):
        self.screen = pyte.Screen(columns, rows)
        self.stream = pyte.Stream(self.screen)
        self.log = (ARTIFACTS / f"{name}.ansi").open("w", encoding="utf-8")
        self.name = name
        environment = {**os.environ, "TERM": "xterm-256color", "COLORTERM": "truecolor"}
        environment.pop("NO_COLOR", None)
        if STANDALONE:
            environment["PATH"] = str(directory / "no-external-programs")
            environment["PHOTOPINFIX_EXIFTOOL"] = "/missing-exiftool"
            environment["PHOTOPINFIX_PYTHON"] = "/missing-python"
            for name in ["PERL5LIB", "PERL5OPT", "PYTHONPATH", "PYTHONHOME", "CONDA_PREFIX", "VIRTUAL_ENV"]:
                environment.pop(name, None)
        self.child = pexpect.spawn(
            str(BINARY), ["--graphics", "halfblocks", str(directory)],
            cwd=directory if STANDALONE else ROOT, encoding="utf-8", codec_errors="replace", timeout=30,
            dimensions=(rows, columns), env=environment,
        )
        # Ratatui asks the terminal for its initial cursor position. A PTY alone
        # is not a terminal emulator; route pyte's CSI replies back to the app.
        self.screen.write_process_input = self.child.send

    def text(self):
        # pyte's display property assumes every wide-character continuation is
        # preceded by its original cell, which differential CJK redraws can
        # invalidate. Read the cells directly, retaining empty continuations.
        return "\n".join(
            "".join(self.screen.buffer[y][x].data for x in range(self.screen.columns))
            for y in range(self.screen.lines)
        )

    def until(self, condition, description, seconds=40):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if condition(self.text()):
                return self.text()
            try:
                data = self.child.read_nonblocking(65536, timeout=max(0.01, deadline - time.monotonic()))
            except pexpect.EOF as exc:
                raise AssertionError(f"TUI exited waiting for {description}:\n{self.text()}") from exc
            except pexpect.TIMEOUT:
                break
            self.log.write(data)
            self.log.flush()
            self.stream.feed(data)
        self.snapshot("failure")
        raise AssertionError(f"Timed out waiting for {description}:\n{self.text()}")

    def expect(self, text, seconds=40):
        # CJK continuation cells can lose a separating blank during pyte's
        # differential redraw. Match visible text without layout whitespace.
        compact = "".join(text.split())
        return self.until(lambda screen: compact in "".join(screen.split()), text, seconds)

    def expect_stats(self, native, auto, unassigned, manual, written):
        for label, count in [("原有GPS", native), ("自动待写", auto), ("待指定", unassigned), ("手动待写", manual), ("已写入", written)]:
            self.expect(f"{label} {count}")

    @staticmethod
    def modal_visible(screen, title):
        # pyte can lose the left corner when it overwrites a CJK continuation
        # cell. Match the full title followed by its horizontal border on the
        # same line; status/help text containing the title has no such border.
        marker = "".join(title.split()) + "──"
        return any(marker in "".join(line.split()) for line in screen.splitlines())

    def expect_modal(self, title, seconds=40):
        return self.until(lambda screen: self.modal_visible(screen, title), f"{title} dialog", seconds)

    def expect_modal_closed(self, title, ready="PhotoPinFix", seconds=40):
        expected = "".join(ready.split())

        def closed(screen):
            compact = "".join(screen.split())
            return not self.modal_visible(screen, title) and expected in compact

        # Background counts/status remain visible under a modal. They cannot
        # acknowledge Esc/Enter by themselves. Also reject an empty clear frame
        # while the terminal is between the modal and its replacement view.
        return self.until(closed, f"{title} closed; {ready}", seconds)

    def expect_selected(self, filename, seconds=40):
        name = "".join(filename.split())
        return self.until(
            lambda screen: any(
                "›" in line and name in "".join(line.split())
                for line in screen.splitlines()
            ),
            f"selected photo {filename}", seconds,
        )

    def key(self, text):
        self.child.send(text)

    def snapshot(self, suffix):
        # Consume the remainder of any large image frame already arriving.
        # Waiting for PTY readability handles split writes without fixed sleeps.
        while select.select([self.child.child_fd], [], [], 0.1)[0]:
            data = self.child.read_nonblocking(65536, timeout=0)
            self.log.write(data)
            self.stream.feed(data)
        (ARTIFACTS / f"{self.name}-{suffix}.txt").write_text(self.text(), encoding="utf-8")
        cells = []
        for y in range(self.screen.lines):
            for x in range(self.screen.columns):
                char = self.screen.buffer[y][x]
                cells.append({"x": x, "y": y, "text": char.data, "fg": char.fg, "bg": char.bg})
        (ARTIFACTS / f"{self.name}-{suffix}.json").write_text(json.dumps(cells, ensure_ascii=False), encoding="utf-8")

    def close(self):
        # Closing a modal can trigger a final clear/cursor query before q is
        # handled. Keep emulating the terminal until the process actually exits.
        deadline = time.monotonic() + 40
        while True:
            try:
                data = self.child.read_nonblocking(65536, timeout=max(0.01, deadline - time.monotonic()))
            except pexpect.EOF:
                break
            self.log.write(data)
            self.stream.feed(data)
            assert time.monotonic() < deadline, "TUI did not finish restoring the terminal"
        self.child.close()
        self.log.close()
        assert self.child.exitstatus == 0, (self.child.exitstatus, self.child.signalstatus)


def main():
    global BINARY
    with tempfile.TemporaryDirectory(prefix="photopinfix-tui-") as temporary, tempfile.TemporaryDirectory(prefix="photopinfix-app-") as runtime:
        if STANDALONE:
            executable = Path(runtime) / BINARY.name
            shutil.copy2(BINARY, executable)
            BINARY = executable
        directory = Path(temporary)
        for source in sorted((ROOT / "demo/photos").iterdir()):
            original = times(source)
            shutil.copy2(source, directory / source.name)
            os.utime(source, ns=original[:2])
        files = sorted(directory.iterdir())
        before = {path.name: times(path) for path in files}

        tui = Tui(directory, "tui-flow")
        tui.expect("找到 73 张照片")
        tui.expect("Session 1/9")
        tui.expect_stats(2, 29, 0, 0, 0)
        tui.expect("31 个位置")
        tui.until(lambda s: "正在读取图片" not in s, "first HEIC preview")
        tui.expect("采用前图")
        tui.expect("时区未知")
        tui.until(lambda screen: "├" in screen and (" m" in screen or " km" in screen), "metric map ruler")
        tui.snapshot("initial")
        tui.key("?")
        tui.expect_modal("快捷键")
        tui.key("\x1b")
        tui.expect_modal_closed("快捷键", "自动待写 29")
        tui.key("]")
        tui.expect_selected(files[1].name)
        tui.key("]")
        tui.expect_selected(files[2].name)
        tui.key("[")
        tui.expect_selected(files[1].name)
        for key, method in [("p", "copy-previous"), ("n", "copy-next"), ("c", "copy-nearest"), ("i", "time-interpolation")]:
            tui.key(key)
            tui.expect(f"手动选择 {method}")
        tui.key("e")
        tui.expect_modal("编辑 GPS · 坐标输入")
        tui.key("\x1533.5\t\x15-70.25")
        tui.key("\x1bOQ")  # xterm F2
        tui.expect("F3 切换")
        tui.expect("33°30′00.000″")
        tui.snapshot("coordinate-editor")
        tui.key("\x1bOR")  # xterm F3: signed to hemispheres
        tui.expect("70°15′00.000″W")
        tui.key("\r")
        tui.expect_modal_closed("编辑 GPS · 坐标输入", "手动坐标已暂存")
        tui.expect_stats(2, 28, 0, 1, 0)
        tui.key("]")
        tui.expect_selected(files[2].name)
        tui.key("[")
        tui.expect_selected(files[1].name)
        tui.key("s")
        tui.expect_modal("Settings · 本次运行")
        tui.snapshot("settings")
        tui.key("\x1bOQ\r")
        tui.expect_modal_closed("Settings · 本次运行", "设置已应用")
        tui.expect("°")
        tui.key("s")
        tui.expect_modal("Settings · 本次运行")
        tui.key("\x1bOQ")
        tui.key("\x159223372036854775807\r")
        tui.expect("过大")
        tui.key("\x1561\t\x15600\r")
        tui.expect_modal_closed("Settings · 本次运行", "设置已应用")
        tui.key("s")
        tui.expect_modal("Settings · 本次运行")
        tui.key("\x1560\r")
        tui.expect_modal_closed("Settings · 本次运行", "设置已应用")
        tui.key("\r")
        tui.expect_modal("确认写入")
        tui.expect("[取消]")
        tui.key("\x1b")
        tui.expect_modal_closed("确认写入", "手动待写 *")
        assert not list(directory.glob("*.photopinfix-original")), "cancel wrote metadata"
        tui.key("\r")
        tui.expect_modal("确认写入")
        tui.key("y")
        tui.expect_modal_closed("确认写入", "成功 1/1")
        tui.expect_stats(2, 28, 0, 0, 1)
        tui.key("W")
        tui.expect_modal("确认写入")
        tui.key("\r")  # default is cancel
        tui.expect_modal_closed("确认写入", "已写入 1")
        assert len(list(directory.glob("*.photopinfix-original"))) == 1
        tui.key("W")
        tui.expect_modal("确认写入")
        tui.key("y")
        tui.expect_modal_closed("确认写入", "成功 28/28", seconds=120)
        tui.expect_stats(2, 0, 0, 0, 29)
        tui.key("\x1b[C")
        tui.expect("Session 2/9")
        tui.expect_stats(3, 3, 5, 0, 0)
        tui.key("a")
        tui.expect("全部照片")
        tui.expect_stats(12, 18, 13, 0, 30)
        tui.key("W")
        tui.expect_modal("确认写入")
        tui.child.setwinsize(24, 80)
        tui.screen.resize(24, 80)
        tui.expect("[确认]")
        tui.snapshot("confirm-80x24")
        tui.child.setwinsize(14, 45)
        tui.screen.resize(14, 45)
        tui.until(lambda s: "[取消]" in s and "[确认]" in s, "small terminal confirm buttons")
        tui.snapshot("confirm-45x14")
        tui.key("\x1b")
        tui.expect_modal_closed("确认写入")
        tui.key("q")
        tui.close()
        for path in files:
            assert times(path) == before[path.name], f"TUI changed file timestamps/identity: {path.name}"
        assert len(list(directory.glob("*.photopinfix-original"))) == 29
        print("PASS: TUI sessions, colors, shortcuts, decimal/DMS editor, temporary settings, confirmations, single/batch write, small terminal controls and file times", flush=True)

        # Start a real scan and quit as soon as the progress frame is visible.
        # Worker shutdown must wait for its access-time guards to run.
        tui = Tui(directory, "tui-early-exit")
        tui.expect("正在扫描")
        tui.key("q")
        tui.close()
        for path in files:
            assert times(path) == before[path.name], f"Early exit changed atime: {path.name}"
        print("PASS: exit during a real metadata scan preserves file times", flush=True)
        if STANDALONE:
            print("PASS: copied executable outside project, empty PATH, no Python/Perl/ExifTool runtime", flush=True)


if __name__ == "__main__":
    main()
