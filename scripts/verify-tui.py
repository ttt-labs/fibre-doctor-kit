"""Exercise the real Windows TUI in ConPTY and photograph its terminal output.

Requires Python with Playwright, an installed Chromium browser, and a local copy
of xterm.js (xterm.js + xterm.css). The browser emulates the bytes emitted by the
running binary; this script does not recreate the application interface in HTML.
Only the repository's synthetic legacy fixtures are opened. No collector or SSH
command is executed. Browser requests are disabled during capture.

Example (PowerShell, after building the Windows release binary):
  .tools/ui-env/Scripts/python.exe scripts/verify-tui.py `
    --binary target/release/doctor-fibre-kit.exe `
    --browser 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe' `
    --xterm-dir .lab/xterm-5.5.0 --out reports/tui-preview

Screenshots are terminal-emulator captures of actual ConPTY output, using
synthetic demonstration data. Keep this provenance with any published images.
"""

import argparse
import base64
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import threading
import time


class Coord(ctypes.Structure):
    _fields_ = [("X", wintypes.SHORT), ("Y", wintypes.SHORT)]


class StartupInfo(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD), ("lpReserved", wintypes.LPWSTR),
        ("lpDesktop", wintypes.LPWSTR), ("lpTitle", wintypes.LPWSTR),
        ("dwX", wintypes.DWORD), ("dwY", wintypes.DWORD),
        ("dwXSize", wintypes.DWORD), ("dwYSize", wintypes.DWORD),
        ("dwXCountChars", wintypes.DWORD), ("dwYCountChars", wintypes.DWORD),
        ("dwFillAttribute", wintypes.DWORD), ("dwFlags", wintypes.DWORD),
        ("wShowWindow", wintypes.WORD), ("cbReserved2", wintypes.WORD),
        ("lpReserved2", ctypes.POINTER(wintypes.BYTE)),
        ("hStdInput", wintypes.HANDLE), ("hStdOutput", wintypes.HANDLE),
        ("hStdError", wintypes.HANDLE),
    ]


class StartupInfoEx(ctypes.Structure):
    _fields_ = [("StartupInfo", StartupInfo), ("lpAttributeList", wintypes.LPVOID)]


class ProcessInfo(ctypes.Structure):
    _fields_ = [
        ("hProcess", wintypes.HANDLE), ("hThread", wintypes.HANDLE),
        ("dwProcessId", wintypes.DWORD), ("dwThreadId", wintypes.DWORD),
    ]


class PseudoConsole:
    """Small bounded ConPTY host; output is continuously drained on a thread."""

    def __init__(self, command, cwd, columns=140, rows=42):
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self._declare_api()
        self.messages = queue.Queue()
        self.raw = bytearray()
        self.read_error = None
        self.process = ProcessInfo()
        self.console = wintypes.HANDLE()
        self.input_read = wintypes.HANDLE()
        self.input_write = wintypes.HANDLE()
        self.output_read = wintypes.HANDLE()
        self.output_write = wintypes.HANDLE()
        self.reader = None
        self.closed = False
        self.write_lock = threading.Lock()
        self.columns, self.rows = columns, rows
        try:
            self._check(self.kernel.CreatePipe(ctypes.byref(self.input_read), ctypes.byref(self.input_write), None, 0))
            self._check(self.kernel.CreatePipe(ctypes.byref(self.output_read), ctypes.byref(self.output_write), None, 0))
            result = self.kernel.CreatePseudoConsole(Coord(columns, rows), self.input_read, self.output_write, 0, ctypes.byref(self.console))
            if result:
                raise OSError(f"CreatePseudoConsole HRESULT: {result:#x}")
            self.reader = threading.Thread(target=self._read, daemon=True)
            self.reader.start()
            size = ctypes.c_size_t()
            self.kernel.InitializeProcThreadAttributeList(None, 1, 0, ctypes.byref(size))
            attributes = ctypes.create_string_buffer(size.value)
            self._check(self.kernel.InitializeProcThreadAttributeList(attributes, 1, 0, ctypes.byref(size)))
            try:
                self._check(self.kernel.UpdateProcThreadAttribute(attributes, 0, 0x00020016, self.console, ctypes.sizeof(self.console), None, None))
                startup = StartupInfoEx()
                startup.StartupInfo.cb = ctypes.sizeof(startup)
                # With redirected parent output, Windows otherwise duplicates
                # the parent's pipe handles instead of connecting the child
                # to ConPTY. Explicit null standard handles let ConPTY supply
                # its console handles (Microsoft terminal discussion #15814).
                startup.StartupInfo.dwFlags = 0x00000100  # STARTF_USESTDHANDLES
                startup.lpAttributeList = ctypes.cast(attributes, wintypes.LPVOID)
                mutable_command = ctypes.create_unicode_buffer(subprocess.list2cmdline([str(item) for item in command]))
                # Describe the terminal that consumes this session, not the
                # redirected host process (which may use TERM=dumb).
                environment = dict(os.environ)
                environment["TERM"] = "xterm-256color"
                environment["COLORTERM"] = "truecolor"
                environment.pop("NO_COLOR", None)
                environment_block = ctypes.create_unicode_buffer("\0".join(
                    f"{key}={value}" for key, value in sorted(environment.items(), key=lambda pair: pair[0].upper())
                ) + "\0\0")
                self._check(self.kernel.CreateProcessW(None, mutable_command, None, None, False, 0x00080400, environment_block, str(cwd), ctypes.byref(startup), ctypes.byref(self.process)))
            finally:
                self.kernel.DeleteProcThreadAttributeList(attributes)
            self.kernel.CloseHandle(self.input_read)
            self.input_read = wintypes.HANDLE()
            self.kernel.CloseHandle(self.output_write)
            self.output_write = wintypes.HANDLE()
        except BaseException:
            self.close()
            raise

    def _declare_api(self):
        handle = wintypes.HANDLE
        pointer = wintypes.LPVOID
        size = ctypes.c_size_t
        definitions = {
            "CreatePipe": ([ctypes.POINTER(handle), ctypes.POINTER(handle), pointer, wintypes.DWORD], wintypes.BOOL),
            "CreatePseudoConsole": ([Coord, handle, handle, wintypes.DWORD, ctypes.POINTER(handle)], ctypes.c_long),
            "ResizePseudoConsole": ([handle, Coord], ctypes.c_long),
            "ClosePseudoConsole": ([handle], None),
            "InitializeProcThreadAttributeList": ([pointer, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(size)], wintypes.BOOL),
            "UpdateProcThreadAttribute": ([pointer, wintypes.DWORD, size, pointer, size, pointer, pointer], wintypes.BOOL),
            "DeleteProcThreadAttributeList": ([pointer], None),
            "CreateProcessW": ([wintypes.LPCWSTR, wintypes.LPWSTR, pointer, pointer, wintypes.BOOL, wintypes.DWORD, pointer, wintypes.LPCWSTR, ctypes.POINTER(StartupInfoEx), ctypes.POINTER(ProcessInfo)], wintypes.BOOL),
            "ReadFile": ([handle, pointer, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), pointer], wintypes.BOOL),
            "WriteFile": ([handle, pointer, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), pointer], wintypes.BOOL),
            "GetExitCodeProcess": ([handle, ctypes.POINTER(wintypes.DWORD)], wintypes.BOOL),
            "WaitForSingleObject": ([handle, wintypes.DWORD], wintypes.DWORD),
            "TerminateProcess": ([handle, wintypes.UINT], wintypes.BOOL),
            "CloseHandle": ([handle], wintypes.BOOL),
        }
        for name, (arguments, result) in definitions.items():
            function = getattr(self.kernel, name)
            function.argtypes, function.restype = arguments, result

    @staticmethod
    def _check(result):
        if not result:
            raise ctypes.WinError(ctypes.get_last_error())

    def _read(self):
        while True:
            buffer = ctypes.create_string_buffer(65536)
            count = wintypes.DWORD()
            if not self.kernel.ReadFile(self.output_read, buffer, len(buffer), ctypes.byref(count), None):
                error = ctypes.get_last_error()
                if error not in (6, 109, 232, 995):
                    self.read_error = f"ReadFile failed: {error}"
                return
            if not count.value:
                return
            data = buffer.raw[:count.value]
            self.raw.extend(data)
            if len(self.raw) > 16 * 1024 * 1024:
                self.read_error = "ConPTY output exceeded 16 MiB capture limit"
                return
            self.messages.put(data)

    def write(self, text):
        data = text.encode("utf-8")
        with self.write_lock:
            count = wintypes.DWORD()
            self._check(self.kernel.WriteFile(self.input_write, data, len(data), ctypes.byref(count), None))
            if count.value != len(data):
                raise OSError("Incomplete ConPTY input write")

    def resize(self, columns, rows):
        result = self.kernel.ResizePseudoConsole(self.console, Coord(columns, rows))
        if result:
            raise OSError(f"ResizePseudoConsole HRESULT: {result:#x}")
        self.columns, self.rows = columns, rows

    def exit_code(self):
        if not self.process.hProcess or self.kernel.WaitForSingleObject(self.process.hProcess, 0) == 258:
            return None
        result = wintypes.DWORD()
        self._check(self.kernel.GetExitCodeProcess(self.process.hProcess, ctypes.byref(result)))
        return result.value

    def close(self):
        if self.closed:
            return
        self.closed = True
        if self.process.hProcess and self.exit_code() is None:
            self.kernel.TerminateProcess(self.process.hProcess, 99)
        if self.console:
            # Keep draining output while ConPTY closes. Bound even cleanup, since
            # an unread output pipe can otherwise deadlock ClosePseudoConsole.
            closer = threading.Thread(target=self.kernel.ClosePseudoConsole, args=(self.console,), daemon=True)
            closer.start()
            closer.join(timeout=3)
            if closer.is_alive():
                self.read_error = "ConPTY cleanup exceeded 3 seconds"
            self.console = wintypes.HANDLE()
        for value in (self.input_read, self.input_write, self.output_write, self.output_read, self.process.hThread, self.process.hProcess):
            if value:
                self.kernel.CloseHandle(value)
        if self.reader:
            self.reader.join(timeout=0.5)


def make_page(context, session, xterm_dir):
    page = context.new_page()
    page.set_content("""<!doctype html><html><head><meta charset="utf-8"><title>ConPTY terminal capture</title>
    <style>html,body{margin:0;background:#0b1220}#terminal-frame{display:inline-block;padding:18px;background:#0b1220;line-height:1}#terminal{display:inline-block}.xterm-viewport{overflow:hidden!important}</style>
    </head><body><div id="terminal-frame"><div id="terminal"></div></div></body></html>""")
    page.add_style_tag(path=str(xterm_dir / "xterm.css"))
    page.add_script_tag(path=str(xterm_dir / "xterm.js"))
    page.expose_function("terminalReply", session.write)
    page.evaluate("""([columns, rows]) => {
      window.term = new Terminal({cols: columns, rows, scrollback: 0,
        fontFamily: "'Cascadia Mono', 'Consolas', monospace", fontSize: 16,
        lineHeight: 1.1, cursorBlink: false, allowProposedApi: true,
        theme: {background:'#0b1220', foreground:'#dbe6f4'}});
      term.onData(data => window.terminalReply(data));
      term.open(document.getElementById('terminal'));
    }""", [session.columns, session.rows])
    return page


def pump(page, session, duration=0.25):
    deadline = time.monotonic() + duration
    while time.monotonic() < deadline:
        if session.read_error:
            raise AssertionError(session.read_error)
        chunks = []
        while True:
            try:
                chunks.append(session.messages.get_nowait())
            except queue.Empty:
                break
        if chunks:
            encoded = base64.b64encode(b"".join(chunks)).decode("ascii")
            page.evaluate("""encoded => new Promise(resolve => {
              const bytes = Uint8Array.from(atob(encoded), c => c.charCodeAt(0));
              term.write(bytes, resolve);
            })""", encoded)
        page.wait_for_timeout(15)


def screen_text(page):
    return page.evaluate(r"""() => Array.from({length:term.rows}, (_, y) =>
      term.buffer.active.getLine(y)?.translateToString(true) || '').join('\n')""")


def wait_screen(page, session, pattern, seconds=8):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        pump(page, session, 0.1)
        text = screen_text(page)
        if re.search(pattern, text, re.IGNORECASE):
            return text
        if session.exit_code() is not None:
            raise AssertionError(f"TUI exited before matching {pattern!r}:\n{text}")
    raise AssertionError(f"TUI did not match {pattern!r} within {seconds} seconds:\n{screen_text(page)}")


def capture(page, session, output, name, key=None, pattern=None):
    if key is not None:
        session.write(key)
    if pattern:
        wait_screen(page, session, pattern)
    pump(page, session, 0.3)
    text = screen_text(page)
    (output / f"{name}.txt").write_text(text, encoding="utf-8")
    page.locator("#terminal-frame").screenshot(path=str(output / f"{name}.png"))
    return text


def check_exit(page, session, key):
    before = len(session.raw)
    session.write(key)
    deadline = time.monotonic() + 5
    while session.exit_code() is None and time.monotonic() < deadline:
        pump(page, session, 0.1)
    pump(page, session, 0.3)
    code = session.exit_code()
    assert code == 0, f"Expected clean TUI exit, got {code}"
    # ConPTY normalizes the application's output, so check terminal state as
    # well as the emitted show-cursor sequence rather than a specific command
    # order or exact byte-for-byte rendering.
    buffer_type = page.evaluate("term.buffer.active.type")
    assert buffer_type == "normal", f"Alternate screen left active: {buffer_type}"
    exit_bytes = bytes(session.raw[before:])
    assert b"\x1b[?25h" in exit_bytes, "No cursor restoration observed during exit"
    return {"exit_code": code, "active_buffer": buffer_type, "cursor_restored": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--browser", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--xterm-dir", required=True, type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("This capture harness uses Windows ConPTY; run it on Windows.")
    for path in (args.binary, args.browser, args.xterm_dir / "xterm.js", args.xterm_dir / "xterm.css"):
        if not path.is_file():
            parser.error(f"Missing required file: {path}")
    from playwright.sync_api import sync_playwright

    root = Path(__file__).resolve().parent.parent
    command = [args.binary.resolve(), "tui", "--snapshot", root / "tests/fixtures/legacy/snapshot-a.json",
               "--compare", root / "tests/fixtures/legacy/snapshot-b.json", "--mode", "temporal",
               "--allow-legacy-identity", "--profile", "v10.4.0-mocha"]
    args.out.mkdir(parents=True, exist_ok=True)
    args.out = args.out.resolve()
    xterm_dir = args.xterm_dir.resolve()
    result = {"provenance": "Real Windows binary output, captured through ConPTY and rendered by local xterm.js",
              "fixture": "Synthetic repository legacy snapshots; no production data or live collection",
              "terminal_environment": {"TERM": "xterm-256color", "COLORTERM": "truecolor", "NO_COLOR": "unset"},
              "initial_size": {"columns": 140, "rows": 42}, "screens": [], "network_requests": []}
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(executable_path=str(args.browser.resolve()), headless=True)
        context = browser.new_context(offline=True, viewport={"width": 1600, "height": 1100}, device_scale_factor=1)
        context.on("request", lambda request: result["network_requests"].append(request.url) if request.url.startswith(("http:", "https:")) else None)
        session = PseudoConsole(command, root)
        try:
            page = make_page(context, session, xterm_dir)
            wait_screen(page, session, r"Fibre")
            for name, key, pattern in [
                ("tui-overview", "1", r"Host memory"),
                ("tui-findings", "2", r"Evidence|Recommendation|Impact"),
                ("tui-capacity", "3", r"Receive.buffer|Connections"),
                ("tui-compare", "4", r"Comparison scope"),
            ]:
                capture(page, session, args.out, name, key, pattern)
                result["screens"].append(name)
            # Arrow navigation changes the selected finding and Tab cycles views.
            session.write("2")
            pump(page, session)
            before_arrow = screen_text(page)
            session.write("\x1b[B")
            pump(page, session)
            assert screen_text(page) != before_arrow, "Down arrow did not change the findings view"
            session.write("\t")
            pump(page, session)
            assert screen_text(page) != before_arrow, "Tab did not change the active view"
            session.resize(80, 24)
            page.evaluate("term.resize(80, 24)")
            capture(page, session, args.out, "tui-compact", "1", r"Fibre")
            result["resize"] = {"columns": 80, "rows": 24, "passed": True}
            result["quit"] = check_exit(page, session, "q")
            (args.out / "session.ansi").write_bytes(bytes(session.raw))
            page.close()
        finally:
            session.close()
        assert not session.read_error, session.read_error
        control_session = PseudoConsole(command, root)
        try:
            page = make_page(context, control_session, xterm_dir)
            wait_screen(page, control_session, r"Fibre")
            result["ctrl_c"] = check_exit(page, control_session, "\x03")
            page.close()
        finally:
            control_session.close()
        assert not control_session.read_error, control_session.read_error
        assert not result["network_requests"], result["network_requests"]
        result["passed"] = True
        browser.close()
    (args.out / "verification.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
