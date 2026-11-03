"""P7 real cancel test. Fresh session, set_model low.
Turn: run `sleep 45`. Auto-allow permission. At +6s (tool running, before
WaitMsBeforeAsync elapses) send session/cancel. Observe stopReason, then
alive-check prompt. Records sleep child pid fate via pgrep before/after.
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
state = json.load(open(os.path.join(outdir, "state.json")))
state["s4"] = sid
json.dump(state, open(os.path.join(outdir, "state.json"), "w"))
send({"jsonrpc": "2.0", "id": 3, "method": "session/set_model", "params": {"sessionId": sid, "modelId": "gemini-3.8-flash-low"}})
time.sleep(1)
t0 = time.time()
send({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {
    "sessionId": sid, "prompt": [{"type": "text", "text": "Run the shell command `sleep 45` then reply with exactly: done. Do not ask questions, just run it."}]}})
answered = []
resp4 = None
sent_cancel = False
# snapshot sleep children of agy before cancel
def sleep_kids():
    try:
        out = subprocess.run(["pgrep", "-f", "sleep 45"], capture_output=True, text=True, timeout=5)
        return sorted(out.stdout.split())
    except Exception as e:
        return [f"pgrep-err {e}"]
end = t0 + 100
while time.time() < end:
    dt = time.time() - t0
    if dt > 6 and not sent_cancel:
        kids_before = sleep_kids()
        send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}})
        print(f"CANCEL sent at +{round(dt,1)}s kids_before={kids_before}", flush=True)
        sent_cancel = True
    with lock: snap = list(frames)
    for _, tag, line in snap:
        if tag != "out": continue
        try: d = json.loads(line)
        except Exception: continue
        if d.get("method") == "session/request_permission" and d.get("id") not in answered:
            answered.append(d.get("id"))
            send({"jsonrpc": "2.0", "id": d.get("id"), "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}})
            print(f"allowed at +{round(dt,1)}s", flush=True)
        if d.get("id") == 4 and ("result" in d or "error" in d) and not resp4:
            resp4 = d
            print(f"RESP4 at +{round(dt,1)}s: {json.dumps(d)[:300]}", flush=True)
    if resp4 and dt > 8:
        break
    time.sleep(0.2)
kids_after = sleep_kids()
print("kids_after_cancel_turn:", kids_after, flush=True)
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
kids_final = sleep_kids()
print("kids_final:", kids_final, flush=True)
# kill leftover sleep children started by this probe only
for k in kids_final:
    if k.strip().isdigit():
        try: os.kill(int(k.strip()), signal.SIGKILL)
        except Exception: pass
print("cleaned leftover sleep kids", flush=True)
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
with open(os.path.join(outdir, "t7b-cancel.jsonl"), "w") as f:
    for ts, tag, line in frames:
        f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
print(f"[t7b] alive_before_eof={alive} exit={proc.returncode} frames={len(frames)}", flush=True)
