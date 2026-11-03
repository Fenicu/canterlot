"""P6 question surface. Fresh session, set_model low.
Prompt asks the model to ask a clarifying question with options.
Auto-answers elicitation/create (accept, first enum values) or ask_question
permission (allow) once; records everything; waits for turn end.
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
    "sessionId": sid, "prompt": [{"type": "text", "text": "Before doing anything, ask me a clarifying question with options: which banner color should I use — red, green or blue? Use the question tool. Do not run any commands."}]}})
seen = set()
resp4 = None
end = t0 + 150
while time.time() < end:
    dt = time.time() - t0
    with lock: snap = list(frames)
    for _, tag, line in snap:
        if tag != "out": continue
        try: d = json.loads(line)
        except Exception: continue
        m = d.get("method")
        if m in ("session/request_permission", "elicitation/create", "fs/read_text_file", "terminal/output") and d.get("id") not in seen:
            seen.add(d.get("id"))
            print(f"SRVREQ {m} id={d.get('id')} at +{round(dt,1)}s", flush=True)
            print(json.dumps(d, indent=1)[:2500], flush=True)
            if m == "elicitation/create":
                schema = ((d.get("params", {}).get("inputSchema")) or {})
                content = {}
                for pn, ps in (schema.get("properties") or {}).items():
                    if isinstance(ps, dict) and "enum" in ps and ps["enum"]:
                        content[pn] = ps["enum"][0]
                    elif isinstance(ps, dict) and ps.get("type") == "string":
                        content[pn] = "probe-answer"
                send({"jsonrpc": "2.0", "id": d.get("id"), "result": {"action": "accept", "content": content}})
                print(f"answered elicitation accept content={content}", flush=True)
        if d.get("id") == 4 and ("result" in d or "error" in d) and not resp4:
            resp4 = d
            print(f"RESP4 at +{round(dt,1)}s: {json.dumps(d)[:300]}", flush=True)
    if resp4:
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
with open(os.path.join(outdir, "t6-question.jsonl"), "w") as f:
    for ts, tag, line in frames:
        f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
print(f"[t6] alive_before_eof={alive} exit={proc.returncode} frames={len(frames)}", flush=True)
