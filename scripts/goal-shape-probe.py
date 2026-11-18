#!/usr/bin/env python3
"""Печать формы кадров цели настоящего CLI. Запускает владелец вручную.

Скрипт НЕ печатает содержимое цели и другие данные владельца: только имена
методов, ключи кадров и известные значения статуса. В конце снимает цель и выходит.

    python3 scripts/goal-shape-probe.py codex      # app-server, без хода модели
    python3 scripts/goal-shape-probe.py claude     # -p stream-json, /goal приведёт к ходу модели

Можно указать бинарники: CODEX_BIN=... CLAUDE_BIN=... . Таймаут задаётся
переменной GOAL_PROBE_TIMEOUT (секунды, по умолчанию 30).
"""

import json
import os
import select
import subprocess
import sys
import time

TIMEOUT = float(os.environ.get("GOAL_PROBE_TIMEOUT", "30"))
STATUSES = {"active", "paused", "blocked", "usageLimited", "budgetLimited", "complete"}
OBJECTIVE = "PROBE_NEUTRAL_OBJECTIVE"


def shape(value):
    """Форма кадра без данных: ключи, типы и известные значения статуса."""
    if isinstance(value, dict):
        out = {}
        for key in sorted(value):
            item = value[key]
            if key in ("status", "state") and isinstance(item, str) and item in STATUSES:
                out[key] = item
            elif isinstance(item, dict):
                out[key] = shape(item)
            elif isinstance(item, list):
                out[key] = [shape(x) if isinstance(x, dict) else type(x).__name__ for x in item[:3]]
            else:
                out[key] = type(item).__name__
        return out
    return type(value).__name__


class Probe:
    def __init__(self, argv):
        self.proc = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self.frames = []

    def send(self, value):
        self.proc.stdin.write(json.dumps(value) + "\n")
        self.proc.stdin.flush()

    def wait(self, wanted, limit=TIMEOUT):
        """Собрать кадры, пока не придёт ответ с нужным id или method."""
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.proc.stdout], [], [], 0.25)
            if not ready:
                continue
            line = self.proc.stdout.readline()
            if not line:
                return False
            try:
                frame = json.loads(line)
            except json.JSONDecodeError:
                continue
            self.frames.append(frame)
            if frame.get("id") == wanted or frame.get("method") == wanted:
                return True
        return False

    def report(self):
        print(json.dumps([shape(frame) for frame in self.frames], ensure_ascii=False, indent=2))

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


def probe_codex():
    binary = os.environ.get("CODEX_BIN", "codex")
    p = Probe([binary, "app-server", "--listen", "stdio://", "--enable", "goals"])
    p.send({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "goal-shape-probe"}}})
    p.wait(1)
    p.send({"method": "initialized", "params": {}})
    p.send({"id": 2, "method": "thread/start", "params": {}})
    if not p.wait(2):
        print("thread/start: no response", file=sys.stderr)
        p.report()
        p.close()
        return 1
    thread = None
    for frame in p.frames:
        if frame.get("id") == 2 and isinstance(frame.get("result"), dict):
            thread = (frame["result"].get("thread") or {}).get("id")
    if not thread:
        print("thread/start: no thread id", file=sys.stderr)
        p.close()
        return 1
    p.send({
        "id": 3,
        "method": "thread/goal/set",
        "params": {"threadId": thread, "objective": OBJECTIVE, "origin": "user"},
    })
    p.wait(3)
    p.send({"id": 4, "method": "thread/goal/get", "params": {"threadId": thread}})
    p.wait(4)
    p.send({"id": 5, "method": "thread/goal/clear", "params": {"threadId": thread, "origin": "user"}})
    p.wait(5)
    p.report()
    p.close()
    return 0


def probe_claude():
    # Пустой MCP-конфиг: только локальная команда /goal, без серверов владельца.
    binary = os.environ.get("CLAUDE_BIN", "claude")
    p = Probe([
        binary, "-p", "--input-format", "stream-json", "--output-format", "stream-json",
        "--verbose", "--include-partial-messages", "--permission-mode", "acceptEdits",
    ])
    p.send({
        "type": "user",
        "message": {"role": "user", "content": "/goal " + OBJECTIVE},
        "parent_tool_use_id": None,
    })
    time.sleep(min(TIMEOUT, 12))
    p.send({
        "type": "user",
        "message": {"role": "user", "content": "/goal clear"},
        "parent_tool_use_id": None,
    })
    time.sleep(min(TIMEOUT, 6))
    p.report()
    p.close()
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in ("codex", "claude"):
        print(__doc__)
        sys.exit(2)
    sys.exit(probe_codex() if sys.argv[1] == "codex" else probe_claude())
