"""No-model-turn ACP probes: P1, P14, P15, P16-setup, P19-flags, P20, P21.
Each scenario spawns its own `agy acp` and logs frames {ts, dir, line}.
Usage: python3 probe_noturn.py <workdir> <outdir>
No credential files are read. Default gemini dir (~/.gemini) is only used
via the agy process itself; only session/list (read-only) runs against it.
"""
import json, os, subprocess, sys, threading, time, signal

AGY = os.path.expanduser("~/.local/bin/agy")
workdir = sys.argv[1]
outdir = sys.argv[2]
os.makedirs(outdir, exist_ok=True)

USAGE_BEFORE = None

def spawn(extra=(), env_extra=None):
    env = dict(os.environ)
    if env_extra:
        env.update(env_extra)
    proc = subprocess.Popen([AGY, "acp", *extra], cwd=workdir,
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, start_new_session=True, env=env)
    return proc

class Session:
    def __init__(self, name, extra=(), env_extra=None):
        self.name = name
        self.frames = []
        self.lock = threading.Lock()
        self.proc = spawn(extra, env_extra)
        print(f"[{name}] pid={self.proc.pid} extra={extra} env={env_extra}", flush=True)
        for stream, tag in ((self.proc.stdout, "out"), (self.proc.stderr, "err")):
            threading.Thread(target=self._reader, args=(stream, tag), daemon=True).start()

    def _reader(self, stream, tag):
        for line in stream:
            line = line.decode(errors="replace").rstrip("\n")
            with self.lock:
                self.frames.append((round(time.time(), 3), tag, line))

    def send(self, obj):
        data = json.dumps(obj)
        with self.lock:
            self.frames.append((round(time.time(), 3), "in", data))
        self.proc.stdin.write((data + "\n").encode())
        self.proc.stdin.flush()

    def wait_id(self, i, timeout=30):
        end = time.time() + timeout
        while time.time() < end:
            with self.lock:
                for _, tag, line in self.frames:
                    if tag == "out":
                        try:
                            d = json.loads(line)
                        except Exception:
                            continue
                        if d.get("id") == i and ("result" in d or "error" in d):
                            return d
            time.sleep(0.1)
        return None

    def out_lines(self):
        with self.lock:
            return [l for _, t, l in self.frames if t == "out"]

    def close(self):
        alive = self.proc.poll() is None
        try:
            self.proc.stdin.close()
        except BrokenPipeError:
            pass
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
        time.sleep(0.3)
        with open(os.path.join(outdir, self.name + ".jsonl"), "w") as f:
            for ts, tag, line in self.frames:
                f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
        print(f"[{self.name}] alive_before_eof={alive} exit={self.proc.returncode} eof_close_s={dt} frames={len(self.frames)}", flush=True)
        return alive

def init(s, i=1):
    s.send({"jsonrpc": "2.0", "id": i, "method": "initialize", "params": {
        "protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False},
        "clientInfo": {"name": "canterlot-probe", "version": "0"}}})
    return s.wait_id(i, 30)

# ---- P1: diagnostics on default dir ----
s = Session("p1-diagnostics")
t0 = time.time()
r = init(s)
print("P1 initialize_s:", round(time.time() - t0, 2), "authMethods:", [m.get("id") for m in (r.get("result", {}).get("authMethods", []) if r else [])], flush=True)
s.send({"jsonrpc": "2.0", "id": 2, "method": "session/list", "params": {}})
r2 = s.wait_id(2, 30)
if r2:
    res = r2.get("result", {})
    sess = res.get("sessions", res)
    n = len(sess) if isinstance(sess, list) else "?"
    print("P1 session/list: ok, count:", n, "keys:", list(res.keys()) if isinstance(res, dict) else type(res), flush=True)
    # redact: keep only ids count, drop titles
    with open(os.path.join(outdir, "p1-session-list-redacted.json"), "w") as f:
        if isinstance(sess, list):
            f.write(json.dumps({"count": len(sess), "ids": [str(x.get("sessionId", "?"))[:8] + "..." for x in sess if isinstance(x, dict)]}) + "\n")
        else:
            f.write(json.dumps({"raw_keys": list(res.keys()) if isinstance(res, dict) else []}) + "\n")
else:
    print("P1 session/list: TIMEOUT", flush=True)
# cold session/new timing
t0 = time.time()
s.send({"jsonrpc": "2.0", "id": 3, "method": "session/new", "params": {"cwd": workdir, "mcpServers": []}})
r3 = s.wait_id(3, 90)
cold = round(time.time() - t0, 2)
sid = (r3.get("result", {}) or {}).get("sessionId") if r3 else None
print(f"P1 cold session/new: {cold}s sid={str(sid)[:8] if sid else None}", flush=True)
if r3 and "result" in r3:
    res = r3["result"]
    print("P1 models:", [m.get("modelId") for m in res.get("models", {}).get("availableModels", [])], "current:", res.get("models", {}).get("currentModelId"), flush=True)
    print("P1 configOptions:", json.dumps(res.get("configOptions"))[:800], flush=True)
    print("P1 _meta:", json.dumps(res.get("_meta"))[:300], flush=True)
# warm session/new timing
t0 = time.time()
s.send({"jsonrpc": "2.0", "id": 4, "method": "session/new", "params": {"cwd": workdir, "mcpServers": []}})
r4 = s.wait_id(4, 90)
print("P1 warm session/new:", round(time.time() - t0, 2), "s", flush=True)
s.close()

# ---- P14: set_model / set_config_option variants (free: no prompt) ----
s = Session("p14-model-effort")
init(s)
s.send({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": workdir, "mcpServers": []}})
r = s.wait_id(2, 90)
sid = (r.get("result", {}) or {}).get("sessionId") if r else None
print("P14 sid:", str(sid)[:8], flush=True)
attempts = [
    (10, "session/set_model", {"sessionId": sid, "modelId": "gemini-3.8-flash-low"}),
    (11, "session/set_config_option", {"sessionId": sid, "id": "thought_level", "value": "low"}),
    (12, "session/set_config_option", {"sessionId": sid, "configId": "thought_level", "value": "low"}),
    (13, "session/set_model", {"sessionId": sid, "model": "gemini-3.8-flash-low"}),
    (14, "session/set_config_option", {"sessionId": sid, "optionId": "model", "value": "gemini-3.8-flash"}),
    (15, "session/set_model", {"sessionId": sid, "modelId": "no-such-model-xyz"}),
]
for i, m, p in attempts:
    s.send({"jsonrpc": "2.0", "id": i, "method": m, "params": p})
    rr = s.wait_id(i, 20)
    body = json.dumps(rr)[:400] if rr else "TIMEOUT"
    print(f"P14 id={i} {m} {json.dumps(p)[:100]} -> {body}", flush=True)
s.close()

# ---- P15: _meta systemPrompt.append in new/load ----
s = Session("p15-instructions")
init(s)
s.send({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {
    "cwd": workdir, "mcpServers": [],
    "_meta": {"antigravity": {"systemPrompt": {"append": "TESTAPPEND canterlot-probe-marker-12345"}}}}})
r = s.wait_id(2, 90)
print("P15 new+append ->", json.dumps(r)[:400] if r else "TIMEOUT", flush=True)
s.close()

# ---- P16-setup: mcpServers variants ----
s = Session("p16-mcp-setup")
init(s)
s.send({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {
    "cwd": workdir,
    "mcpServers": [{"command": "true", "args": [], "env": []}]}})
r = s.wait_id(2, 90)
print("P16 dummy mcp ->", json.dumps(r)[:500] if r else "TIMEOUT", flush=True)
s.close()

# ---- P19: flags accepted ----
for extra, nm in ((("--dangerously-skip-permissions",), "p19-skip-perms"),
                  (("--disable-telemetry",), "p19-no-telemetry"),
                  (("--sandbox",), "p19-sandbox")):
    s = Session(nm, extra=extra)
    r = init(s, 20)
    print(nm, "->", "ok" if r and "result" in r else json.dumps(r)[:200] if r else "TIMEOUT/ERR", flush=True)
    s.close()

# ---- P22 pre-turn: unknown method probe (capability discovery, free) ----
s = Session("p22-caps")
init(s)
for i, m, p in ((30, "session/capabilities", {}),
                (31, "session/methods", {}),
                (32, "session/set_mode", {"sessionId": "x", "modeId": "plan"})):
    s.send({"jsonrpc": "2.0", "id": i, "method": m, "params": p})
    rr = s.wait_id(i, 15)
    print(f"P22 id={i} {m} ->", json.dumps(rr)[:300] if rr else "TIMEOUT", flush=True)
s.close()
print("NOTURN DONE", flush=True)
