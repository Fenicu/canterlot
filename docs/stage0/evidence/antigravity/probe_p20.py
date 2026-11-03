"""P20 (auth isolation + concurrency) and P21 (updater) probes. No turns.
No credential files are read; isolated dirs live under /tmp.
"""
import json, os, subprocess, sys, threading, time, signal

AGY = os.path.expanduser("~/.local/bin/agy")
workdir = sys.argv[1]
outdir = sys.argv[2]
os.makedirs(outdir, exist_ok=True)

def run_session(name, extra=(), env_extra=None, script=None, timeout_each=25):
    """script: list of (id_or_None, method, params). None id = notification."""
    proc = subprocess.Popen([AGY, "acp", *extra], cwd=workdir, stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            start_new_session=True,
                            env={**os.environ, **(env_extra or {})})
    print(f"[{name}] pid={proc.pid} extra={extra} env={env_extra}", flush=True)
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
    def wait_id(i, timeout=timeout_each):
        end = time.time() + timeout
        while time.time() < end:
            with lock:
                for _, tag, line in frames:
                    if tag == "out":
                        try: d = json.loads(line)
                        except Exception: continue
                        if d.get("id") == i and ("result" in d or "error" in d):
                            return d
            time.sleep(0.1)
        return None
    results = {}
    for i, m, p in script:
        if i is None:
            send({"jsonrpc": "2.0", "method": m, "params": p})
        else:
            send({"jsonrpc": "2.0", "id": i, "method": m, "params": p})
            results[i] = wait_id(i)
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
    dt = round(time.time() - t0, 2)
    time.sleep(0.3)
    with open(os.path.join(outdir, name + ".jsonl"), "w") as f:
        for ts, tag, line in frames:
            f.write(json.dumps({"ts": ts, "dir": tag, "line": line}) + "\n")
    print(f"[{name}] alive_before_eof={alive} exit={proc.returncode} eof_close_s={dt} frames={len(frames)}", flush=True)
    return results

INIT = {"protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False},
        "clientInfo": {"name": "canterlot-probe", "version": "0"}}

# P20a: isolated gemini dir
iso = "/tmp/agy-stage0-iso1"
os.makedirs(iso, exist_ok=True)
r = run_session("p20a-isolated", extra=(f"--gemini-dir={iso}",),
                script=[(1, "initialize", INIT),
                        (2, "session/new", {"cwd": workdir, "mcpServers": []})])
print("P20a init:", json.dumps(r[1])[:200], flush=True)
print("P20a new:", json.dumps(r[2])[:300], flush=True)
# names only of what was created (temp dir, no credentials read)
for root, dirs, files in os.walk(iso):
    rel = os.path.relpath(root, iso)
    print("P20a created:", rel, "dirs=", dirs[:10], "files=", [f for f in files[:10]], flush=True)
    if rel.count("/") >= 3: break

# P20c: GEMINI_FORCE_FILE_STORAGE=true on isolated dir
r = run_session("p20c-force-file-storage", extra=(f"--gemini-dir={iso}",),
                env_extra={"GEMINI_FORCE_FILE_STORAGE": "true"},
                script=[(1, "initialize", INIT),
                        (2, "session/new", {"cwd": workdir, "mcpServers": []})])
print("P20c new:", json.dumps(r[2])[:300], flush=True)

# P20b: two processes on the SAME isolated dir concurrently
procs = []
for k in ("A", "B"):
    p = subprocess.Popen([AGY, "acp", f"--gemini-dir={iso}"], cwd=workdir,
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, start_new_session=True)
    procs.append((k, p))
    print(f"[p20b-{k}] pid={p.pid}", flush=True)
time.sleep(1.5)
both_alive = all(p.poll() is None for _, p in procs)
print("P20b both alive after 1.5s:", both_alive, flush=True)
for k, p in procs:
    p.stdin.close()
    try: p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL); p.wait()
    print(f"[p20b-{k}] exit={p.returncode}", flush=True)
with open(os.path.join(outdir, "p20b-concurrency.txt"), "w") as f:
    f.write(f"both_alive_after_1.5s={both_alive}\n")

# P20d: two session/list on DEFAULT dir concurrently (read-only)
def list_default(tag, bag):
    p = subprocess.Popen([AGY, "acp"], cwd=workdir, stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                         start_new_session=True)
    def send(o):
        p.stdin.write((json.dumps(o) + "\n").encode()); p.stdin.flush()
    send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": INIT})
    # read until id 1 answered
    buf = b""
    ok = False
    end = time.time() + 20
    while time.time() < end:
        line = p.stdout.readline()
        if not line: break
        try: d = json.loads(line)
        except Exception: continue
        if d.get("id") == 1 and "result" in d:
            ok = True; break
    cnt = None
    if ok:
        send({"jsonrpc": "2.0", "id": 2, "method": "session/list", "params": {}})
        end = time.time() + 20
        while time.time() < end:
            line = p.stdout.readline()
            if not line: break
            try: d = json.loads(line)
            except Exception: continue
            if d.get("id") == 2 and "result" in d:
                s = d["result"].get("sessions", [])
                cnt = len(s); break
    p.stdin.close()
    try: p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL); p.wait()
    bag[tag] = (ok, cnt, p.returncode)
bag = {}
ta = threading.Thread(target=list_default, args=("A", bag))
tb = threading.Thread(target=list_default, args=("B", bag))
ta.start(); tb.start(); ta.join(); tb.join()
print("P20d concurrent list on default dir:", bag, flush=True)
with open(os.path.join(outdir, "p20d-concurrency.txt"), "w") as f:
    f.write(json.dumps(bag) + "\n")

# P21: updater traces (no network-heavy action; just help/strings evidence + env check)
u = subprocess.run([AGY, "update", "--help"], capture_output=True, text=True, timeout=30)
print("P21 update --help:", repr((u.stdout + u.stderr)[:500]), flush=True)
with open(os.path.join(outdir, "p21-update-help.txt"), "w") as f:
    f.write(u.stdout + u.stderr)
print("P20/P21 DONE", flush=True)
