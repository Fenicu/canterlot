"""P5 deny/allow + P4 completion. Usage: probe_p5.py <workdir> <outdir> <deny|allow>
Loads session s2, set_model low, sends a run_command turn, auto-answers the
first session/request_permission with deny/allow, waits for turn end.
"""
import json, os, subprocess, sys, threading, time, signal

AGY = os.path.expanduser("~/.local/bin/agy")
workdir, outdir, mode = sys.argv[1], sys.argv[2], sys.argv[3]
assert mode in ("deny", "allow")
os.makedirs(outdir, exist_ok=True)
state = json.load(open(os.path.join(outdir, "state.json")))
sid = state["s2"]

proc = subprocess.Popen([AGY, "acp"], cwd=workdir, stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                        start_new_session=True)
print("pid", proc.pid, "mode", mode, "sid", sid[:8], flush=True)
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
send({"jsonrpc": "2.0", "id": 2, "method": "session/load", "params": {"sessionId": sid, "cwd": workdir, "mcpServers": []}})
time.sleep(2)
send({"jsonrpc": "2.0", "id": 3, "method": "session/set_model", "params": {"sessionId": sid, "modelId": "gemini-3.8-flash-low"}})
time.sleep(1)
cmd = "echo perm-deny-probe" if mode == "deny" else "echo perm-allow-probe"
send({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {
    "sessionId": sid, "prompt": [{"type": "text", "text": f"Run the shell command `{cmd}` then reply with exactly: done. Do not ask questions, just run it."}]}})
answered = []
t0 = time.time()
end = t0 + 150
resp4 = None
while time.time() < end:
    with lock:
        snap = list(frames)
    for _, tag, line in snap:
        if tag != "out":
            continue
        try: d = json.loads(line)
        except Exception: continue
        if d.get("method") == "session/request_permission" and d.get("id") not in answered:
            rid = d["id"]
            answered.append(rid)
            print(f"PERMREQ id={rid} opts={json.dumps(d['params'].get('options'))}", flush=True)
            send({"jsonrpc": "2.0", "id": rid, "result": {"outcome": {"outcome": "selected", "optionId": mode}}})
            print(f"ANSWERED {mode} at +{round(time.time()-t0,1)}s", flush=True)
        if d.get("id") == 4 and ("result" in d or "error" in d):
            resp4 = d
    if resp4:
        break
    time.sleep(0.2)
print("TURN:", json.dumps(resp4)[:400] if resp4 else "TIMEOUT", f"dt={round(time.time()-t0,1)}s", flush=True)
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
time.sleep(0.3)
with open(os.path.join(outdir, f"t5-{mode}.jsonl"), "w") as f:
    for ts, tag, line in frames:
        f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
print(f"[t5-{mode}] alive_before_eof={alive} exit={proc.returncode} frames={len(frames)}", flush=True)
