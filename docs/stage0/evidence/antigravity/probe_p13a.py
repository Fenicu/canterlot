"""P13 SIGKILL during tool turn. Fresh session, set_model low.
Turn: run `sleep 60`. Auto-allow. At +6s: SIGKILL the agy process group.
Then: pgrep sleep leftovers; new process + session/load + alive prompt.
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
stop = False
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
json.dump({"s6": sid}, open(os.path.join(outdir, "state-p13.json"), "w"))
send({"jsonrpc": "2.0", "id": 3, "method": "session/set_model", "params": {"sessionId": sid, "modelId": "gemini-3.8-flash-low"}})
time.sleep(1)
t0 = time.time()
send({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {
    "sessionId": sid, "prompt": [{"type": "text", "text": "Run the shell command `sleep 60` then reply with exactly: done. Do not ask questions, just run it."}]}})
allowed = False
killed = False
end = t0 + 60
while time.time() < end:
    dt = time.time() - t0
    with lock: snap = list(frames)
    for _, tag, line in snap:
        if tag != "out": continue
        try: d = json.loads(line)
        except Exception: continue
        if d.get("method") == "session/request_permission" and not allowed:
            allowed = True
            send({"jsonrpc": "2.0", "id": d.get("id"), "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}})
            print(f"allowed at +{round(dt,1)}s", flush=True)
    if allowed and dt > 6 and not killed:
        killed = True
        print(f"SIGKILL pgid at +{round(dt,1)}s", flush=True)
        with open(os.path.join(outdir, "t13-kill.jsonl"), "w") as f:
            for ts, tag, line in frames:
                f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
        os.killpg(proc.pid, signal.SIGKILL)
        break
    time.sleep(0.2)
time.sleep(1)
print("agy alive after SIGKILL:", proc.poll() is None, "rc=", proc.poll(), flush=True)
r = subprocess.run(["pgrep", "-af", "sleep 60"], capture_output=True, text=True, timeout=10)
print("sleep leftovers:", repr(r.stdout.strip()), flush=True)
print("[t13a] pre-kill frames saved", flush=True)
