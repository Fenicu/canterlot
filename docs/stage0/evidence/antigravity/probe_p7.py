"""P7 cancel + P3 steer-in-one-turn. Fresh session, set_model low.
Turn: run `sleep 45`. At +8s send a second prompt (P3: observe queue/error/merge).
At +14s send session/cancel (P7). Then send a follow-up prompt to prove process alive.
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

INIT = {"protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False},
        "clientInfo": {"name": "canterlot-probe", "version": "0"}}
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": INIT})
time.sleep(1)
send({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": workdir, "mcpServers": []}})
# wait for new
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
state = json.load(open(os.path.join(outdir, "state.json"))) if os.path.exists(os.path.join(outdir, "state.json")) else {}
state["s3"] = sid
json.dump(state, open(os.path.join(outdir, "state.json"), "w"))
send({"jsonrpc": "2.0", "id": 3, "method": "session/set_model", "params": {"sessionId": sid, "modelId": "gemini-3.8-flash-low"}})
time.sleep(1)
t0 = time.time()
send({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {
    "sessionId": sid, "prompt": [{"type": "text", "text": "Run the shell command `sleep 45` then reply with exactly: done. Do not ask questions, just run it."}]}})
answered = []
resp4 = None
sent_p3 = False
sent_cancel = False
sent_p4 = False
resp5 = None
end = t0 + 120
while time.time() < end:
    dt = time.time() - t0
    if dt > 8 and not sent_p3:
        send({"jsonrpc": "2.0", "id": 5, "method": "session/prompt", "params": {
            "sessionId": sid, "prompt": [{"type": "text", "text": "P3-PROBE second prompt during turn"}]}})
        print(f"P3 second prompt sent at +{round(dt,1)}s", flush=True)
        sent_p3 = True
    if dt > 14 and not sent_cancel:
        send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}})
        print(f"CANCEL sent at +{round(dt,1)}s", flush=True)
        sent_cancel = True
    with lock: snap = list(frames)
    for _, tag, line in snap:
        if tag != "out": continue
        try: d = json.loads(line)
        except Exception: continue
        if d.get("method") == "session/request_permission" and d.get("id") not in answered:
            answered.append(d.get("id"))
            print(f"PERMREQ id={d.get('id')}", flush=True)
            send({"jsonrpc": "2.0", "id": d.get("id"), "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}})
            print("answered allow", flush=True)
        if d.get("id") == 4 and ("result" in d or "error" in d) and not resp4:
            resp4 = d
            print(f"RESP4 at +{round(dt,1)}s: {json.dumps(d)[:300]}", flush=True)
        if d.get("id") == 5 and ("result" in d or "error" in d) and not resp5:
            resp5 = d
            print(f"RESP5(P3) at +{round(dt,1)}s: {json.dumps(d)[:300]}", flush=True)
    if resp4 and sent_p3 and (resp5 or dt > 40):
        break
    time.sleep(0.2)
# follow-up prompt proves process alive (P7)
if proc.poll() is None:
    send({"jsonrpc": "2.0", "id": 6, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": "Reply with exactly: alive"}]}})
    end2 = time.time() + 90
    while time.time() < end2:
        with lock: snap = list(frames)
        for _, tag, line in snap:
            if tag != "out": continue
            try: d = json.loads(line)
            except Exception: continue
            if d.get("id") == 6 and ("result" in d or "error" in d):
                print(f"RESP6(alive-check): {json.dumps(d)[:300]}", flush=True)
                end2 = 0; break
        if end2 == 0: break
        time.sleep(0.3)
alive = proc.poll() is None
try: proc.stdin.close()
except BrokenPipeError: pass
t1 = time.time()
try: proc.wait(timeout=10)
except subprocess.TimeoutExpired:
    os.killpg(proc.pid, signal.SIGTERM)
    try: proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL); proc.wait()
# check sleep child survived?
time.sleep(0.3)
with open(os.path.join(outdir, "t7-cancel-p3-steer.jsonl"), "w") as f:
    for ts, tag, line in frames:
        f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
print(f"[t7] alive_before_eof={alive} exit={proc.returncode} frames={len(frames)}", flush=True)
