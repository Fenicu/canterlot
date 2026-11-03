"""Model-turn ACP probes. Usage: python3 probe_turns.py <workdir> <outdir> <stage>
Stages (each reuses prior session id saved in outdir/state.json when possible):
  t2_text    P2: set_model low + text turn, echo delay, stopReason, _meta/usage
  t22_image  P22: prompt with image block (may fail before a turn -> free)
  t4_tool    P4: run_command echo + nonzero exit, tool_call frames
  t5_perm    P5: ask model to request approval; harness auto-answers reject_once
  t7_cancel  P7: start sleep turn, session/cancel mid-turn
State file keeps session ids. Every turn is short and uses gemini-3.8-flash-low.
"""
import json, os, subprocess, sys, threading, time, signal

AGY = os.path.expanduser("~/.local/bin/agy")
workdir, outdir, stage = sys.argv[1], sys.argv[2], sys.argv[3]
os.makedirs(outdir, exist_ok=True)
STATE = os.path.join(outdir, "state.json")
state = json.load(open(STATE)) if os.path.exists(STATE) else {}

proc = subprocess.Popen([AGY, "acp"], cwd=workdir, stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                        start_new_session=True)
print("pid", proc.pid, flush=True)
frames, lock = [], threading.Lock()
def reader(stream, tag):
    for line in stream:
        line = line.decode(errors="replace").rstrip("\n")
        with lock:
            frames.append((round(time.time(), 3), tag, line))
for st, tag in ((proc.stdout, "out"), (proc.stderr, "err")):
    threading.Thread(target=reader, args=(st, tag), daemon=True).start()

def send(obj):
    data = json.dumps(obj)
    with lock:
        frames.append((round(time.time(), 3), "in", data))
    proc.stdin.write((data + "\n").encode()); proc.stdin.flush()

next_id = [100]
def rpc(method, params, timeout=120, note=""):
    i = next_id[0]; next_id[0] += 1
    t0 = time.time()
    send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
    end = t0 + timeout
    while time.time() < end:
        with lock:
            for _, tag, line in frames:
                if tag == "out":
                    try: d = json.loads(line)
                    except Exception: continue
                    if d.get("id") == i and ("result" in d or "error" in d):
                        print(f"[{stage}{note}] {method} id={i} dt={round(time.time()-t0,2)}s -> {json.dumps(d)[:600]}", flush=True)
                        return d
        time.sleep(0.2)
    print(f"[{stage}{note}] {method} id={i} TIMEOUT", flush=True)
    return None

def notify(method, params):
    send({"jsonrpc": "2.0", "method": method, "params": params})

INIT = {"protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False},
        "clientInfo": {"name": "canterlot-probe", "version": "0"}}

def ensure_session(key):
    if key in state:
        return state[key]
    rpc("initialize", INIT, timeout=30)
    r = rpc("session/new", {"cwd": workdir, "mcpServers": []}, timeout=90)
    sid = r["result"]["sessionId"]
    state[key] = sid
    json.dump(state, open(STATE, "w"))
    return sid

def set_low(sid):
    r = rpc("session/set_model", {"sessionId": sid, "modelId": "gemini-3.8-flash-low"}, timeout=30)
    return r

if stage == "t2_text":
    sid = ensure_session("s1")
    set_low(sid)
    t0 = time.time(); rpc("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "Reply with exactly: apple"}]}, timeout=180)
    print(f"[t2] turn dt={round(time.time()-t0,2)}s", flush=True)
elif stage == "t22_image":
    sid = ensure_session("simg")
    set_low(sid)
    px = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
    t0 = time.time()
    rpc("session/prompt", {"sessionId": sid, "prompt": [
        {"type": "text", "text": "Reply with exactly: apple"},
        {"type": "image", "data": px, "mimeType": "image/png"}]}, timeout=180)
    print(f"[t22] turn dt={round(time.time()-t0,2)}s", flush=True)
elif stage == "t4_tool":
    sid = ensure_session("s2")
    set_low(sid)
    t0 = time.time()
    rpc("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "Run the shell command `echo probe-hi; exit 3` in MODEL, then reply with exactly: done. Do not ask questions, just run it."}]}, timeout=180)
    print(f"[t4] turn dt={round(time.time()-t0,2)}s", flush=True)

alive = proc.poll() is None
try: proc.stdin.close()
except BrokenPipeError: pass
t0 = time.time()
try: proc.wait(timeout=10)
except subprocess.TimeoutExpired:
    os.killpg(proc.pid, signal.SIGTERM)
    try: proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL); proc.wait()
time.sleep(0.3)
with open(os.path.join(outdir, stage + ".jsonl"), "w") as f:
    for ts, tag, line in frames:
        f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
print(f"[{stage}] alive_before_eof={alive} exit={proc.returncode} eof_close_s={round(time.time()-t0,2)} frames={len(frames)}", flush=True)
