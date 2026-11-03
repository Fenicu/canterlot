import json, os, subprocess, sys, threading, time, signal

workdir = sys.argv[1]
out_path = sys.argv[2]
extra = sys.argv[3:]
proc = subprocess.Popen(["agy", "acp", *extra], cwd=workdir, stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
print("pid", proc.pid, flush=True)
frames = []
lock = threading.Lock()

def reader(stream, tag):
    for line in stream:
        line = line.decode(errors="replace").rstrip("\n")
        with lock:
            frames.append((round(time.time(), 3), tag, line))

threading.Thread(target=reader, args=(proc.stdout, "out"), daemon=True).start()
threading.Thread(target=reader, args=(proc.stderr, "err"), daemon=True).start()

def send(obj):
    data = json.dumps(obj) + "\n"
    with lock:
        frames.append((round(time.time(), 3), "in", data.strip()))
    proc.stdin.write(data.encode()); proc.stdin.flush()

def wait_id(i, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        with lock:
            for _, tag, line in frames:
                if tag == "out":
                    try:
                        d = json.loads(line)
                    except Exception:
                        continue
                    if d.get("id") == i and ("result" in d or "error" in d):
                        return d
        time.sleep(0.1)
    return None

send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": 1,
    "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False},
    "clientInfo": {"name": "canterlot-probe", "version": "0"}}})
init = wait_id(1)
sid = None
if init and "result" in init:
    send({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": workdir, "mcpServers": []}})
    new = wait_id(2, 60)
    if new and "result" in new:
        sid = new["result"].get("sessionId")
if sid:
    send({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {
        "sessionId": sid, "prompt": [{"type": "text", "text": "Reply with exactly: apple"}]}})
    wait_id(3, 40)
    send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}})
    time.sleep(2)
    send({"jsonrpc": "2.0", "id": 4, "method": "session/load", "params": {"sessionId": sid, "cwd": workdir, "mcpServers": []}})
    wait_id(4, 30)
    send({"jsonrpc": "2.0", "id": 5, "method": "session/load", "params": {"sessionId": "11111111-2222-3333-4444-555555555555", "cwd": workdir, "mcpServers": []}})
    wait_id(5, 30)
time.sleep(1)
alive = proc.poll() is None
proc.stdin.close()
t0 = time.time()
try:
    proc.wait(timeout=10)
except subprocess.TimeoutExpired:
    os.killpg(proc.pid, signal.SIGTERM)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait()
print("alive_before_eof", alive, "exit", proc.returncode, "exit_after_eof_s", round(time.time() - t0, 2), flush=True)
time.sleep(0.5)
with open(out_path, "w") as f:
    for ts, tag, line in frames:
        f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
