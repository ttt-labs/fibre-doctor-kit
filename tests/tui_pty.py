"""Exercise the real Linux TUI in a PTY using synthetic snapshots only.

Run from any directory: python3 tests/tui_pty.py PATH/TO/fibre-doctor-kit
No live collection, network access, service changes, or third-party packages.
"""

import codecs
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import termios
import time
import unicodedata


class Screen:
    """Small VT screen model for the cursor positioning emitted by Crossterm."""

    def __init__(self, columns, rows):
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.pending = ""
        self.alternate = False
        self.cursor_visible = True
        self.entered_alternate = False
        self.cursor_reports = []
        self.saved_position = (0, 0)
        self.resize(columns, rows)

    def resize(self, columns, rows):
        self.columns, self.rows = columns, rows
        self.cells = [[" "] * columns for _ in range(rows)]
        self.row, self.column = 0, 0

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.cells)

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            value = self.pending[0]
            consumed = 1
            if value == "\x1b":
                if len(self.pending) < 2:
                    return
                if self.pending[1] == "[":
                    end = next((i for i in range(2, len(self.pending))
                                if "@" <= self.pending[i] <= "~"), None)
                    if end is None:
                        return
                    self.csi(self.pending[2:end], self.pending[end])
                    consumed = end + 1
                elif self.pending[1] == "]":
                    endings = [(self.pending.find("\x07"), 1),
                               (self.pending.find("\x1b\\"), 2)]
                    endings = [(index, width) for index, width in endings if index >= 0]
                    if not endings:
                        return
                    end, width = min(endings)
                    consumed = end + width
                elif self.pending[1] in "()":
                    if len(self.pending) < 3:
                        return
                    consumed = 3
                else:
                    consumed = 2
            elif value == "\r":
                self.column = 0
            elif value == "\n":
                self.row = min(self.rows - 1, self.row + 1)
            elif value == "\b":
                self.column = max(0, self.column - 1)
            elif value >= " " and value != "\x7f":
                if unicodedata.combining(value):
                    if self.column > 0:
                        self.cells[self.row][self.column - 1] += value
                else:
                    width = 2 if unicodedata.east_asian_width(value) in "WF" else 1
                    if self.column >= self.columns:
                        self.row = min(self.rows - 1, self.row + 1)
                        self.column = 0
                    self.cells[self.row][self.column] = value
                    if width == 2 and self.column + 1 < self.columns:
                        self.cells[self.row][self.column + 1] = ""
                    self.column += width
            self.pending = self.pending[consumed:]

    def csi(self, parameters, command):
        private = parameters.startswith("?")
        raw = parameters.lstrip("?=>")
        try:
            values = [int(value or 0) for value in raw.split(";")]
        except ValueError:
            return
        first = values[0]
        amount = first or 1
        if command in "Hf":
            self.row = min(self.rows - 1, max(0, amount - 1))
            self.column = min(self.columns - 1, max(0, (values[1] or 1) - 1)) if len(values) > 1 else 0
        elif command in "ABCD":
            if command == "A":
                self.row = max(0, self.row - amount)
            elif command == "B":
                self.row = min(self.rows - 1, self.row + amount)
            elif command == "C":
                self.column = min(self.columns - 1, self.column + amount)
            else:
                self.column = max(0, self.column - amount)
        elif command == "G":
            self.column = min(self.columns - 1, max(0, amount - 1))
        elif command == "d":
            self.row = min(self.rows - 1, max(0, amount - 1))
        elif command == "J" and first in (2, 3):
            self.cells = [[" "] * self.columns for _ in range(self.rows)]
        elif command == "K":
            start = self.column if first == 0 else 0
            end = self.column + 1 if first == 1 else self.columns
            self.cells[self.row][start:end] = [" "] * (end - start)
        elif command == "s":
            self.saved_position = self.row, self.column
        elif command == "u":
            self.row, self.column = self.saved_position
        elif command in "hl" and private:
            if 1049 in values:
                self.alternate = command == "h"
                self.entered_alternate |= self.alternate
            if 25 in values:
                self.cursor_visible = command == "h"
        elif command == "n" and first == 6:
            self.cursor_reports.append(f"\x1b[{self.row + 1};{self.column + 1}R".encode())


class Session:
    def __init__(self, binary, columns=140, rows=42):
        fixtures = Path(__file__).resolve().parent / "fixtures" / "legacy"
        args = [str(binary), "tui", "--snapshot", str(fixtures / "snapshot-a.json"),
                "--compare", str(fixtures / "snapshot-b.json"), "--mode", "temporal",
                "--allow-legacy-identity", "--profile", "v10.4.0-mocha"]
        self.master, self.slave = pty.openpty()
        self.original_terminal = termios.tcgetattr(self.slave)
        self.screen = Screen(columns, rows)
        self.output_bytes = 0
        self.deadline = time.monotonic() + 30
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        os.set_blocking(self.master, False)

        def attach_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
            os.tcsetpgrp(0, os.getpgrp())

        try:
            self.process = subprocess.Popen(
                args, stdin=self.slave, stdout=self.slave, stderr=self.slave,
                preexec_fn=attach_terminal,
                env={**os.environ, "TERM": "xterm-256color", "LANG": "C.UTF-8"},
            )
        except BaseException:
            os.close(self.master)
            os.close(self.slave)
            raise

    def pump(self, timeout=0.05):
        if time.monotonic() >= self.deadline:
            raise AssertionError("PTY session exceeded its 30-second deadline")
        readable, _, _ = select.select([self.master], [], [], timeout)
        if not readable:
            return
        try:
            data = os.read(self.master, 65536)
        except OSError as error:
            if error.errno in (errno.EIO, errno.EAGAIN):
                return
            raise
        self.output_bytes += len(data)
        if self.output_bytes > 4 * 1024 * 1024:
            raise AssertionError("TUI output exceeded the 4 MiB test limit")
        self.screen.feed(data)
        while self.screen.cursor_reports:
            self.send(self.screen.cursor_reports.pop(0))

    def send(self, data):
        deadline = min(self.deadline, time.monotonic() + 2)
        while data:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError("Timed out writing to PTY")
            _, writable, _ = select.select([], [self.master], [], min(remaining, 0.05))
            if writable:
                try:
                    data = data[os.write(self.master, data):]
                except BlockingIOError:
                    pass

    def expect(self, text, timeout=5):
        deadline = min(self.deadline, time.monotonic() + timeout)
        while time.monotonic() < deadline:
            self.pump()
            if text in self.screen.text():
                return
            if self.process.poll() is not None:
                break
        raise AssertionError(f"Expected visible text {text!r}; terminal:\n{self.screen.text()}")

    def resize(self, columns, rows):
        self.screen.resize(columns, rows)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        os.killpg(self.process.pid, signal.SIGWINCH)

    def expect_clean_exit(self):
        deadline = min(self.deadline, time.monotonic() + 5)
        while self.process.poll() is None and time.monotonic() < deadline:
            self.pump()
        # Drain already-written output without depending on EOF: the parent
        # deliberately holds the slave open to inspect restored termios state.
        for _ in range(64):
            if not select.select([self.master], [], [], 0)[0]:
                break
            self.pump(0)
        assert self.process.poll() == 0, (
            f"TUI did not exit cleanly (status={self.process.poll()}):\n{self.screen.text()}"
        )
        assert self.screen.entered_alternate, "TUI never entered the alternate screen"
        assert not self.screen.alternate, "TUI left the alternate screen enabled"
        assert self.screen.cursor_visible, "TUI left the terminal cursor hidden"
        assert termios.tcgetattr(self.slave) == self.original_terminal, "TUI did not restore termios"

    def close(self):
        try:
            if self.process.poll() is None:
                os.killpg(self.process.pid, signal.SIGKILL)
                try:
                    self.process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    print("PTY cleanup: child did not exit within one second", file=sys.stderr)
        finally:
            os.close(self.master)
            os.close(self.slave)


def main():
    if len(sys.argv) != 2 or not sys.platform.startswith("linux"):
        raise SystemExit("Usage on Linux: python3 tests/tui_pty.py PATH/TO/fibre-doctor-kit")
    binary = Path(sys.argv[1]).resolve(strict=True)
    session = Session(binary)
    try:
        session.expect("FIBRE DOCTOR KIT")
        for key, heading in [(b"2", "Selected finding"), (b"3", "RECEIVE-BUFFER MODEL"),
                             (b"4", "Comparison scope"), (b"1", "Host memory")]:
            session.send(key)
            session.expect(heading)
        session.resize(80, 24)
        session.expect("FIBRE DOCTOR KIT")
        session.resize(48, 12)
        session.expect("Resize terminal")
        session.resize(140, 42)
        session.expect("Host memory")
        session.send(b"q")
        session.expect_clean_exit()
    finally:
        session.close()
    print("PASS: real PTY navigation, resizing, and q restore terminal state")

    for label, action in [("Ctrl+C", b"\x03"), ("SIGTERM", signal.SIGTERM),
                          ("SIGHUP", signal.SIGHUP)]:
        session = Session(binary)
        try:
            session.expect("FIBRE DOCTOR KIT")
            if isinstance(action, bytes):
                session.send(action)
            else:
                os.killpg(session.process.pid, action)
            session.expect_clean_exit()
        finally:
            session.close()
        print(f"PASS: {label} exits promptly and restores terminal state")


if __name__ == "__main__":
    main()
