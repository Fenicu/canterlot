"""Live probes against the real `agy acp` after the quota reset (04.11.2026).

Scenarios (argv[1]): questions, mcp, mcp-resume, cache, shell, usage.
Usage: python3 probe_live.py <scenario> <workdir> <outdir>

Frames are saved redacted as <outdir>/p-live-<scenario>.jsonl in the stage-0
format {"ts", "dir", "line"}. No credential files are read. The agent's own
files under ~/.gemini are only touched by the agy process itself.
"""
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time

AGY = os.path.expanduser("~/.local/bin/agy")
HERE = os.path.dirname(os.path.abspath(__file__))
MCP_SCRIPT = os.path.join(HERE, "mcp_echo.py")

scenario = sys.argv[1]
workdir = sys.argv[2]
outdir = sys.argv[3]
os.makedirs(outdir, exist_ok=True)


def redact(line):
    line = re.sub(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", "<email>", line)
    line = re.sub(r"AIza[0-9A-Za-z_\-]{20,}", "<api-key>", line)
    line = re.sub(r"ya29\.[0-9A-Za-z_\-]+", "<oauth-token>", line)
    line = re.sub(
        r'("[^"]*(?:token|secret|api_key|apiKey|password)[^"]*"\s*:\s*)"[^"]*"',
        r'\1"<redacted>"', line, flags=re.I)
    # session/list carries the owner's conversation titles and cwds: keep neither.
    line = re.sub(r'"title"\s*:\s*"[^"]*"', '"title":"<redacted>"', line)
    line = re.sub(r'("cwd"\s*:\s*)"/(?:home|Users)/[^"]*"', r'\1"<owner-path>"', line)
    return line


class Session:
    def __init__(self, name, extra=()):
        self.name = name
        self.frames = []
        self.lock = threading.Lock()
        self.perm_requests = []
        self.other_requests = []
        self.next_id = 100
        env = dict(os.environ)
        env["GEMINI_FORCE_FILE_STORAGE"] = "true"
        self.proc = subprocess.Popen(
            [AGY, "acp", "--disable-telemetry", *extra], cwd=workdir,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, start_new_session=True, env=env)
        print(f"[{name}] pid={self.proc.pid}", flush=True)
        for stream, tag in ((self.proc.stdout, "out"), (self.proc.stderr, "err")):
            threading.Thread(target=self._reader, args=(stream, tag), daemon=True).start()

    def _record(self, tag, line):
        with self.lock:
            self.frames.append((round(time.time(), 3), tag, line))

    def _reader(self, stream, tag):
        for raw in stream:
            line = raw.decode(errors="replace").rstrip("\n")
            self._record(tag, line)
            if tag != "out":
                continue
            try:
                msg = json.loads(line)
            except Exception:
                continue
            if "method" in msg and "id" in msg:
                self._handle_request(msg)

    def _handle_request(self, msg):
        method = msg.get("method", "")
        params = msg.get("params") or {}
        if method == "session/request_permission":
            options = params.get("options") or []
            choice = None
            for opt in options:
                if str(opt.get("kind", "")).startswith("reject"):
                    choice = opt
                    break
            if choice is None:
                choice = next((o for o in options if str(o.get("optionId", "")).startswith("deny")), None)
            option_id = (choice or {}).get("optionId")
            with self.lock:
                self.perm_requests.append({"id": msg.get("id"), "params": params})
            self._record("err", f"PROBE denied permission request id={msg.get('id')} option={option_id}")
            self.send({"jsonrpc": "2.0", "id": msg.get("id"),
                       "result": {"outcome": {"outcome": "selected", "optionId": option_id}}})
        else:
            with self.lock:
                self.other_requests.append({"id": msg.get("id"), "method": method, "params": params})
            self._record("err", f"PROBE unhandled agent request {method} id={msg.get('id')}")

    def send(self, obj):
        data = json.dumps(obj, ensure_ascii=False)
        self._record("in", data)
        try:
            self.proc.stdin.write((data + "\n").encode())
            self.proc.stdin.flush()
        except (BrokenPipeError, ValueError) as exc:
            self._record("err", f"PROBE write failed: {exc}")

    def request(self, i, method, params, timeout=60):
        self.send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        return self.wait_id(i, timeout)

    def wait_id(self, i, timeout=60):
        end = time.time() + timeout
        while time.time() < end:
            with self.lock:
                snapshot = list(self.frames)
            for _, tag, line in snapshot:
                if tag != "out":
                    continue
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                if d.get("id") == i and ("result" in d or "error" in d):
                    return d
            time.sleep(0.05)
        return None

    def updates(self):
        out = []
        with self.lock:
            snapshot = list(self.frames)
        for _, tag, line in snapshot:
            if tag != "out":
                continue
            try:
                d = json.loads(line)
            except Exception:
                continue
            if d.get("method") == "session/update":
                out.append(d.get("params", {}).get("update", {}))
        return out

    def scrub_result(self, i, summary):
        """Replace a response frame with a summary that carries no owner data."""
        with self.lock:
            for idx, (ts, tag, line) in enumerate(self.frames):
                if tag != "out":
                    continue
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                if d.get("id") == i and "result" in d:
                    self.frames[idx] = (ts, tag, json.dumps(
                        {"jsonrpc": "2.0", "id": i, "result": summary}))

    def last_frame_age(self):
        with self.lock:
            if not self.frames:
                return None
            return round(time.time() - self.frames[-1][0], 2)

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        alive = self.proc.poll() is None
        t0 = time.time()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(self.proc.pid, signal.SIGTERM)
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait()
        dt = round(time.time() - t0, 2)
        time.sleep(0.2)
        path = os.path.join(outdir, f"p-live-{self.name}.jsonl")
        with open(path, "w") as f:
            for ts, tag, line in self.frames:
                f.write(json.dumps({"ts": ts, "dir": tag, "line": redact(line)}) + "\n")
        print(f"[{self.name}] alive_before_eof={alive} exit={self.proc.returncode} "
              f"eof_close_s={dt} frames={len(self.frames)} perms={len(self.perm_requests)} "
              f"other_requests={len(self.other_requests)} -> {path}", flush=True)

    def init(self, i=1):
        r = self.request(i, "initialize", {
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False},
                                   "terminal": False},
            "clientInfo": {"name": "canterlot-probe", "version": "0"}}, timeout=30)
        print(f"[{self.name}] initialize:", "ok" if r and "result" in r else json.dumps(r)[:200],
              flush=True)
        return r

    def summarize_turn(self, response, since=0):
        """Print a compact view of the turn: text, tools, usage, stopReason."""
        if not response:
            print(f"[{self.name}] turn: TIMEOUT", flush=True)
            return
        if "error" in response:
            print(f"[{self.name}] turn error:", json.dumps(response["error"])[:500], flush=True)
            return
        res = response.get("result", {})
        ups = [u for u in self.updates()]
        tools = [u for u in ups if u.get("sessionUpdate") in ("tool_call", "tool_call_update")]
        texts = "".join((u.get("content") or {}).get("text", "")
                        for u in ups if u.get("sessionUpdate") == "agent_message_chunk")
        print(f"[{self.name}] stopReason={res.get('stopReason')} usage={json.dumps(res.get('usage'))}",
              flush=True)
        print(f"[{self.name}] tool frames={len(tools)} perm_requests={len(self.perm_requests)}", flush=True)
        for u in tools:
            print(f"[{self.name}]   tool {u.get('sessionUpdate')} name={u.get('name')} "
                  f"id={u.get('toolCallId')} status={u.get('status')} "
                  f"rawInput={json.dumps(u.get('rawInput'), ensure_ascii=False)[:400]} "
                  f"rawOutput={json.dumps(u.get('rawOutput'), ensure_ascii=False)[:300]}", flush=True)
        print(f"[{self.name}] text: {texts[:800]}", flush=True)


def do_questions():
    s = Session("question")
    s.init()
    r = s.request(2, "session/new", {"cwd": workdir, "mcpServers": []}, timeout=60)
    sid = (r.get("result") or {}).get("sessionId")
    print("question session:", str(sid)[:8], flush=True)
    q_text = ("Before doing anything, ask me which of two options I prefer using your "
              "question tool: the banner color, red or blue. Offer both options, then wait "
              "for my answer and stop.")
    sent = time.time()
    s.send({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": q_text}]}})
    # Wait window: how long does a pending question stay unanswered (P6)?
    window = 90
    t_seen = None
    while time.time() - sent < window:
        ups = s.updates()
        if t_seen is None and any(u.get("sessionUpdate") == "tool_call" and u.get("name") == "ask_question" for u in ups):
            t_seen = time.time()
        time.sleep(0.2)
    print(f"[question] pending window={window}s tool_seen={t_seen is not None} "
          f"last_frame_age={s.last_frame_age()}s", flush=True)
    s.send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}})
    cancelled = s.wait_id(3, 20)
    s.summarize_turn(cancelled)
    cont = ('Ответ владельца на вопрос "Which banner color do you prefer, red or blue?": '
            "Blue. Now reply with exactly: DONE-BLUE and stop.")
    s.send({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": cont}]}})
    resumed = s.wait_id(4, 120)
    s.summarize_turn(resumed)
    s.close()


def do_question_prompt():
    """Answer a pending ask_question with a plain session/prompt, without cancel."""
    s = Session("question-prompt")
    s.init()
    r = s.request(2, "session/new", {"cwd": workdir, "mcpServers": []}, timeout=60)
    sid = (r.get("result") or {}).get("sessionId")
    print("question-prompt session:", str(sid)[:8], flush=True)
    q_text = ("Before doing anything, ask me which of two options I prefer using your "
              "question tool: the banner color, red or blue. Offer both options, then wait "
              "for my answer and stop.")
    s.send({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": q_text}]}})
    deadline = time.time() + 60
    while time.time() < deadline:
        if any(u.get("sessionUpdate") == "tool_call" and u.get("name") == "ask_question"
               for u in s.updates()):
            break
        time.sleep(0.2)
    else:
        print("[question-prompt] no ask_question in 60s", flush=True)
    time.sleep(1)
    cont = ('Ответ владельца на вопрос "Which banner color do you prefer, red or blue?": '
            "Blue. Now reply with exactly: DONE-BLUE and stop.")
    s.send({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": cont}]}})
    first = None
    deadline = time.time() + 120
    while time.time() < deadline:
        r3 = s.wait_id(3, 0.01)
        r4 = s.wait_id(4, 0.01)
        if r3 is not None and first is None:
            first = "id3"
            print("[question-prompt] first prompt (3) answered:", json.dumps(r3)[:300], flush=True)
        if r4 is not None:
            if first is None:
                print("[question-prompt] second prompt (4) answered first", flush=True)
            print("[question-prompt] answer prompt (4):", json.dumps(r4)[:600], flush=True)
            if r3 is None:
                print("[question-prompt] original prompt (3) still open", flush=True)
                r3 = s.wait_id(3, 10)
                print("[question-prompt] original prompt (3) later:", json.dumps(r3)[:400], flush=True)
            break
        time.sleep(0.05)
    else:
        print("[question-prompt] TIMEOUT waiting for either prompt", flush=True)
    s.summarize_turn(s.wait_id(3, 1))
    s.summarize_turn(s.wait_id(4, 1))
    s.close()


def do_question_cancel():
    """Spec §6 workaround: cancel as soon as the question appears, then a service prompt."""
    s = Session("question-cancel")
    s.init()
    r = s.request(2, "session/new", {"cwd": workdir, "mcpServers": []}, timeout=60)
    sid = (r.get("result") or {}).get("sessionId")
    print("question-cancel session:", str(sid)[:8], flush=True)
    q_text = ("Before doing anything, ask me which of two options I prefer using your "
              "question tool: the banner color, red or blue. Offer both options, then wait "
              "for my answer and stop.")
    s.send({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": q_text}]}})
    deadline = time.time() + 60
    while time.time() < deadline:
        if any(u.get("sessionUpdate") == "tool_call" and u.get("name") == "ask_question"
               for u in s.updates()):
            break
        time.sleep(0.1)
    else:
        print("[question-cancel] no ask_question in 60s", flush=True)
    time.sleep(0.5)
    s.send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}})
    r3 = s.wait_id(3, 20)
    print("[question-cancel] prompt after cancel:", json.dumps(r3)[:300] if r3 else "TIMEOUT", flush=True)
    cont = ('Ответ владельца на вопрос "Which banner color do you prefer, red or blue?": '
            "Blue. Now reply with exactly: DONE-BLUE and stop.")
    r4 = s.request(4, "session/prompt", {"sessionId": sid, "prompt": [
        {"type": "text", "text": cont}]}, timeout=120)
    print("[question-cancel] service prompt:", json.dumps(r4)[:600] if r4 else "TIMEOUT", flush=True)
    s.summarize_turn(r4)
    s.close()


def do_mcp():
    args = [MCP_SCRIPT]
    s = Session("mcp1")
    s.init()
    servers = [{"name": "echo-probe", "command": sys.executable, "args": args, "env": []}]
    r = s.request(2, "session/new", {"cwd": workdir, "mcpServers": servers}, timeout=90)
    sid = (r.get("result") or {}).get("sessionId")
    print("mcp session:", str(sid)[:8], flush=True)
    text = ("Two tasks, then stop. First, in one line list the names of every MCP tool you "
            "currently have available (tool identifiers only). Second, call the echo tool with "
            "the text pong-7391, then remember the secret phrase BANANA-4417 and reply with it. "
            "Do not run shell commands and do not edit any files.")
    s.send({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": text}]}})
    resp = s.wait_id(3, 150)
    s.summarize_turn(resp)
    if sid:
        with open(os.path.join(outdir, "p-live-state.json"), "w") as f:
            json.dump({"sessionId": sid}, f)
    s.close()


def do_mcp_resume():
    with open(os.path.join(outdir, "p-live-state.json")) as f:
        sid = json.load(f)["sessionId"]
    servers = [{"name": "echo-probe", "command": sys.executable, "args": [MCP_SCRIPT], "env": []}]
    s = Session("mcp2")
    s.init()
    lst = s.request(2, "session/list", {}, timeout=30)
    sessions = (lst.get("result") or {}).get("sessions") if lst else None
    found = any(isinstance(x, dict) and x.get("sessionId") == sid for x in (sessions or [])) \
        if isinstance(sessions, list) else None
    print("resume: list_ok=", lst is not None, "found=", found,
          "count=", len(sessions) if isinstance(sessions, list) else "?", flush=True)
    s.scrub_result(2, {"sessions_count": len(sessions) if isinstance(sessions, list) else None,
                       "contains_target": found})
    r = s.request(3, "session/resume", {
        "sessionId": sid, "cwd": workdir, "mcpServers": servers,
        "_meta": {"antigravity": {"systemPrompt": {"append":
            "Instruction append marker: always end every reply with the exact token RESUME-APPEND-7F."}}}},
        timeout=60)
    print("resume result:", json.dumps(r)[:400], flush=True)
    text = ("Three things, then stop. What was the secret phrase I told you earlier? List the "
            "names of every MCP tool you have available in one line. Call the echo tool once "
            "with the text after-resume-1187.")
    s.send({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": text}]}})
    resp = s.wait_id(4, 150)
    s.summarize_turn(resp)
    s.close()


def do_cache():
    s = Session("cache")
    s.init()
    r = s.request(2, "session/new", {"cwd": workdir, "mcpServers": []}, timeout=60)
    sid = (r.get("result") or {}).get("sessionId")
    print("cache session:", str(sid)[:8], flush=True)
    filler = " ".join(
        f"Sentence {i}: the antigravity cache probe keeps this line distinct and long."
        for i in range(1, 361))  # ~24 KB, comfortably above the implicit-cache floor
    print("cache filler bytes:", len(filler.encode()), flush=True)
    text1 = ("Here is reference text for a later question. Read it and keep it.\n\n"
             + filler + "\n\nReply with exactly: ALPHA-1")
    s.send({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": text1}]}})
    r1 = s.wait_id(3, 150)
    s.summarize_turn(r1)
    s.send({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": "Reply with exactly: BETA-2"}]}})
    r2 = s.wait_id(4, 150)
    s.summarize_turn(r2)
    s.close()


def do_shell():
    s = Session("shell", extra=("--dangerously-skip-permissions",))
    s.init()
    r = s.request(2, "session/new", {"cwd": workdir, "mcpServers": []}, timeout=60)
    sid = (r.get("result") or {}).get("sessionId")
    text = ("Run exactly one shell command: sh -c 'exit 3'. Then report its exit code in one "
            "line and stop. Do not edit any files.")
    s.send({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": text}]}})
    resp = s.wait_id(3, 150)
    s.summarize_turn(resp)
    s.close()


{
    "questions": do_questions,
    "question-prompt": do_question_prompt,
    "question-cancel": do_question_cancel,
    "mcp": do_mcp,
    "mcp-resume": do_mcp_resume,
    "cache": do_cache,
    "shell": do_shell,
}[scenario]()
