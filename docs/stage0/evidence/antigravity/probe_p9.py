"""P9 subagent visibility. Fresh session, set_model low.
Turn: ask model to delegate `echo subagent-hi` to a subagent and report back.
Auto-allow run_command permissions. Record child linkage frames.
"""
import json, os, subprocess, sys, threading, time, signal

AGY = os.path.expanduser("~/.local/bin/agy")
workdir, outdir = sys.argv[1], sys.argv[2]
os.makedirs(outdir, exist_ok=True)

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

send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": 1,
    "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False},
    "clientInfo": {"name": "canterlot-probe", "version": "0"}}})
time.sleep(1)
send({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": workdir, "mcpServers": []}})
sid = None
end = time.time() + 90
while time.time() < end and not sid:
    with lock: snap = list(frames)
    for _, tag, line in snap:
        if tag == "out":
            try: d = json.loads(line)
            except Exception: continue
            if d.get("id") == 2 and "result" in d:
                sid = d["result"]["sessionId"]; break
    time.sleep(0.2)
print("sid", str(sid)[:8], flush=True)
send({"jsonrpc": "2.0", "id": 3, "method": "session/set_model", "params": {"sessionId": sid, "modelId": "gemini-3.8-flash-low"}})
time.sleep(1)
t0 = time.time()
send({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {
    "sessionId": sid, "prompt": [{"type": "text", "text": "Delegate to a subagent: have it run the shell command `echo subagent-hi` and report the output back to you. Then reply with exactly: done. Do not ask questions."}]}})
seen = set(); resp = None
end = t0 + 240
while time.time() < end:
    dt = time.time() - t0
    with lock: snap = list(frames)
    for _, tag, line in snap:
        if tag != "out": continue
        try: d = json.loads(line)
        except Exception: continue
        m = d.get("method")
        if m == "session/request_permission" and d.get("id") not in seen:
            seen.add(d.get("id"))
            print(f"PERMREQ +{round(dt,1)}s tool={d.get('params',{}).get('toolCall',{}).get('name')} title={d.get('params',{}).get('toolCall',{}).get('title')}", flush=True)
            send({"jsonrpc": "2.0", "id": d.get("id"), "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}})
        if d.get("id") == 4 and ("result" in d or "error" in d) and not resp:
            resp = d
            print(f"RESP +{round(dt,1)}s: {json.dumps(d)[:250]}", flush=True)
    if resp:
        break
    time.sleep(0.3)
alive = proc.poll() is None
try: proc.stdin.close()
except BrokenPipeError: pass
try: proc.wait(timeout=10)
except subprocess.TimeoutExpired:
    os.killpg(proc.pid, signal.SIGTERM)
    try: proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL); proc.wait()
time.sleep(0.3)
with open(os.path.join(outdir, "t9-subagent.jsonl"), "w") as f:
    for ts, tag, line in frames:
        f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
print(f"[t9] alive_before_eof={alive} exit={proc.returncode} frames={len(frames)}", flush=True)
