"""P7-cancel-during-permission. Fresh session, set_model low.
Turn: run `echo cancel-perm-probe`. On request_permission: send session/cancel
instead of answering. Observe turn end + any permission resolution. Alive-check after.
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
    "sessionId": sid, "prompt": [{"type": "text", "text": "Run the shell command `echo cancel-perm-probe` then reply with exactly: done. Do not ask questions, just run it."}]}})
cancelled = False
resp4 = None
end = t0 + 100
while time.time() < end:
    dt = time.time() - t0
    with lock: snap = list(frames)
    for _, tag, line in snap:
        if tag != "out": continue
        try: d = json.loads(line)
        except Exception: continue
        if d.get("method") == "session/request_permission" and not cancelled:
            cancelled = True
            print(f"PERMREQ at +{round(dt,1)}s, sending CANCEL instead of answer", flush=True)
            send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}})
        if d.get("id") == 4 and ("result" in d or "error" in d) and not resp4:
            resp4 = d
            print(f"RESP4 at +{round(dt,1)}s: {json.dumps(d)[:300]}", flush=True)
    if resp4 and dt > 12:
        break
    time.sleep(0.2)
if proc.poll() is None:
    send({"jsonrpc": "2.0", "id": 6, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": "Reply with exactly: alive"}]}})
    end2 = time.time() + 90
    while time.time() < end2:
        with lock: snap = list(frames)
        done = False
        for _, tag, line in snap:
            if tag != "out": continue
            try: d = json.loads(line)
            except Exception: continue
            if d.get("id") == 6 and ("result" in d or "error" in d):
                print(f"RESP6(alive-check): {json.dumps(d)[:300]}", flush=True)
                done = True; break
        if done: break
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
with open(os.path.join(outdir, "t7c-cancel-perm.jsonl"), "w") as f:
    for ts, tag, line in frames:
        f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
print(f"[t7c] alive_before_eof={alive} exit={proc.returncode} frames={len(frames)}", flush=True)
