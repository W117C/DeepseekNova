#!/usr/bin/env python3
"""Generate real TUI screenshots for README.

Runs `deepseeknova-cli chat --tui` inside a PTY against a local mock
OpenAI-compatible server, captures ANSI frames with pyte, and renders PNGs
with Pillow.

Usage:
    python3 scripts/tui-screenshot.py \
        --bin target/debug/deepseeknova-cli \
        --out docs/screenshots

Requirements: `pip install pyte pillow`
"""

import argparse
import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from PIL import Image, ImageDraw, ImageFont
import pyte


COLS, ROWS = 100, 30


def start_mock_server():
    """Minimal OpenAI-compatible SSE endpoint that always answers politely."""

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def _send_json(self, obj):
            body = json.dumps(obj).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path == "/v1/models":
                self._send_json(
                    {"object": "list", "data": [{"id": "deepseek-chat", "object": "model"}]}
                )
            else:
                self.send_response(404)
                self.end_headers()

        def do_POST(self):
            length = int(self.headers.get("Content-Length", 0))
            body = json.loads(self.rfile.read(length) or b"{}")
            stream = body.get("stream", False)
            reply = (
                "你好，我是 DeepseekNova！我可以帮你写代码、查资料、"
                "运行命令，也可以把任务派给专门的子代理。"
            )
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            if stream:
                for i, ch in enumerate(reply):
                    payload = {
                        "id": "chatcmpl-mock",
                        "object": "chat.completion.chunk",
                        "choices": [
                            {
                                "index": 0,
                                "delta": {"content": ch},
                                "finish_reason": None,
                            }
                        ],
                    }
                    self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
                    self.wfile.flush()
                done = {
                    "id": "chatcmpl-mock",
                    "object": "chat.completion.chunk",
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                }
                self.wfile.write(f"data: {json.dumps(done, ensure_ascii=False)}\n\n".encode())
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            else:
                self._send_json(
                    {
                        "id": "chatcmpl-mock",
                        "object": "chat.completion",
                        "choices": [
                            {"index": 0, "message": {"role": "assistant", "content": reply}}
                        ],
                    }
                )

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return port


def write_config(directory, port):
    cfg = f"""
default_model = "deepseek-chat"

[[providers]]
name = "mock"
kind = "openai-compatible"
base_url = "http://127.0.0.1:{port}"
api_key = "sk-mock"
model = "deepseek-chat"

[ui]
lang = "zh"
"""
    with open(os.path.join(directory, "deepseeknova.toml"), "w") as f:
        f.write(cfg)


class AnsiSession:
    """PTY session feeding output into a pyte screen."""

    def __init__(self, argv, cwd, env):
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.master, slave = pty.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", ROWS, COLS, 0, 0),
        )
        env = dict(env)
        env.update({"TERM": "xterm-256color", "LANG": "C.UTF-8", "COLUMNS": str(COLS), "LINES": str(ROWS)})
        self.proc = subprocess.Popen(
            argv,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            cwd=cwd,
            env=env,
            close_fds=True,
            start_new_session=True,
        )
        os.close(slave)

    def read_until_quiet(self, quiet_seconds=1.5, timeout=30):
        deadline = time.time() + timeout
        last_data = time.time()
        while time.time() < deadline:
            r, _, _ = select.select([self.master], [], [], 0.2)
            if r:
                try:
                    data = os.read(self.master, 65536)
                except OSError:
                    break
                if data:
                    self.stream.feed(data)
                    last_data = time.time()
            elif time.time() - last_data >= quiet_seconds:
                return
        raise TimeoutError("TUI did not settle within timeout")

    def read_for(self, seconds):
        """Drain PTY output for a fixed duration (TUI redraws continuously)."""
        deadline = time.time() + seconds
        while time.time() < deadline:
            r, _, _ = select.select([self.master], [], [], 0.2)
            if r:
                try:
                    data = os.read(self.master, 65536)
                except OSError:
                    break
                if data:
                    self.stream.feed(data)

    def type(self, text):
        os.write(self.master, text.encode())

    def stop(self):
        try:
            os.killpg(self.proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        os.close(self.master)


ANSI_COLORS = {
    "default": (201, 209, 217),
    "black": (13, 17, 23),
    "red": (255, 123, 114),
    "green": (63, 185, 80),
    "yellow": (210, 153, 34),
    "blue": (88, 166, 255),
    "magenta": (188, 103, 255),
    "cyan": (58, 166, 210),
    "white": (230, 237, 243),
    "ansibrightblack": (110, 118, 129),
    "ansibrightred": (255, 163, 153),
    "ansibrightgreen": (110, 231, 136),
    "ansibrightyellow": (229, 194, 104),
    "ansibrightblue": (135, 196, 255),
    "ansibrightmagenta": (205, 141, 255),
    "ansibrightcyan": (124, 212, 255),
    "ansibrightwhite": (255, 255, 255),
}


def render_png(screen, path):
    try:
        font = ImageFont.truetype("/System/Library/Fonts/STHeiti Light.ttc", 16)
    except OSError:
        font = ImageFont.load_default()
    cell_w, cell_h = 16, 20
    img = Image.new(
        "RGB",
        (COLS * cell_w, ROWS * cell_h),
        (13, 17, 23),
    )
    draw = ImageDraw.Draw(img)
    for y in range(ROWS):
        for x in range(COLS):
            cell = screen.buffer[y][x]
            bg_name = str(cell.bg).lower()
            fg_name = str(cell.fg).lower()
            bg = (13, 17, 23) if bg_name in ("default", "black") else ANSI_COLORS.get(
                bg_name, (13, 17, 23)
            )
            fg = (201, 209, 217) if fg_name == "default" else ANSI_COLORS.get(
                fg_name, (201, 209, 217)
            )
            draw.rectangle(
                (x * cell_w, y * cell_h, (x + 1) * cell_w, (y + 1) * cell_h),
                fill=bg,
            )
            ch = getattr(cell, "data", getattr(cell, "ch", ""))
            if ch and not ch.isspace():
                draw.text((x * cell_w + 1, y * cell_h), ch, font=font, fill=fg)
    img.save(path)


def screen_lines(screen):
    """Safe text dump: pyte's `display` can crash on wide-char continuations."""
    lines = []
    for y in range(screen.lines):
        row = []
        for x in range(screen.columns):
            cell = screen.buffer[y][x]
            ch = getattr(cell, "data", getattr(cell, "ch", "")) or ""
            row.append(ch)
        lines.append("".join(row).rstrip())
    return lines


def dump_text(screen, path):
    with open(path, "w") as f:
        f.write("\n".join(screen_lines(screen)))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/debug/deepseeknova-cli")
    ap.add_argument("--out", default="docs/screenshots")
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)
    port = start_mock_server()
    workdir = tempfile.mkdtemp(prefix="dnv-shot-")
    write_config(workdir, port)

    env = dict(os.environ)
    env["HOME"] = workdir
    session = AnsiSession(
        [os.path.abspath(args.bin), "chat", "--tui"],
        cwd=workdir,
        env=env,
    )
    try:
        session.read_for(3.0)
        render_png(session.screen, os.path.join(args.out, "tui-welcome.png"))
        dump_text(session.screen, os.path.join(args.out, "tui-welcome.txt"))

        session.type("你好，介绍一下你自己\r")
        session.read_for(5.0)
        render_png(session.screen, os.path.join(args.out, "tui-chat.png"))
        dump_text(session.screen, os.path.join(args.out, "tui-chat.txt"))
    finally:
        session.stop()

    print(f"screenshots written to {args.out}")


if __name__ == "__main__":
    sys.exit(main())
