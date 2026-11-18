#!/usr/bin/env python3
"""Печать формы кадров цели настоящего CLI. Запускает владелец вручную.

Скрипт НЕ печатает содержимое цели и другие данные владельца: только имена
методов, ключи кадров и известные значения статуса. В конце снимает цель и выходит.

    python3 scripts/goal-shape-probe.py codex      # app-server, без хода модели
    python3 scripts/goal-shape-probe.py claude     # -p stream-json, /goal приведёт к ходу модели

Вариант `claude` запускайте из пустого временного каталога: без `--strict-mcp-config`
CLI подхватил бы серверы MCP владельца и стал бы менять его файлы. Здесь MCP отключён,
режим разрешений `default`, число ходов ограничено двумя — правки файлов не производятся.

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
    try:
        # app-server 0.169 требует version и experimentalApi, иначе цель не включается.
        p.send({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {"name": "goal-shape-probe", "version": "1.0.0"},
                "capabilities": {"experimentalApi": True},
            },
        })
        p.wait(1)
        p.send({"method": "initialized", "params": {}})
        p.send({"id": 2, "method": "thread/start", "params": {"cwd": os.getcwd()}})
        if not p.wait(2):
            print("thread/start: no response", file=sys.stderr)
            return 1
        thread = None
        for frame in p.frames:
            if frame.get("id") == 2 and isinstance(frame.get("result"), dict):
                thread = (frame["result"].get("thread") or {}).get("id")
        if not thread:
            print("thread/start: no thread id", file=sys.stderr)
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
        return 0
    finally:
        # Формы уже пришедших кадров нужны и при отказе: по ним видно, где оборвался зонд.
        p.report()
        p.close()


def probe_claude():
    # Запускать из пустого временного каталога: strict MCP и пустой конфиг не дают
    # CLI подключить серверы владельца, режим default и два хода не правят файлы.
    binary = os.environ.get("CLAUDE_BIN", "claude")
    p = Probe([
        binary, "-p", "--input-format", "stream-json", "--output-format", "stream-json",
        "--verbose", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}',
        "--permission-mode", "default", "--max-turns", "2",
    ])
    try:
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
        return 0
    finally:
        p.report()
        p.close()


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in ("codex", "claude"):
        print(__doc__)
        sys.exit(2)
    sys.exit(probe_codex() if sys.argv[1] == "codex" else probe_claude())
