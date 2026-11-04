"""Minimal stdio MCP server for the live P16 probe: one tool `echo(text)`.

Newline-delimited JSON-RPC 2.0 on stdin/stdout. No files, no network.
Usage: python3 mcp_echo.py
"""
import json
import sys

PROTOCOL = "2025-06-18"


def reply(msg_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result}) + "\n")
    sys.stdout.flush()


def error(msg_id, code, message):
    sys.stdout.write(json.dumps({
        "jsonrpc": "2.0", "id": msg_id,
        "error": {"code": code, "message": message},
    }) + "\n")
    sys.stdout.flush()


TOOL = {
    "name": "echo",
    "description": "Echo the given text back, prefixed with ECHO-PROBE.",
    "inputSchema": {
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"],
    },
}

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    method = msg.get("method")
    msg_id = msg.get("id")
    if msg_id is None:
        continue  # notification (initialized, cancelled, ...)
    if method == "initialize":
        reply(msg_id, {
            "protocolVersion": PROTOCOL,
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": {"name": "echo-probe", "version": "1"},
        })
    elif method == "ping":
        reply(msg_id, {})
    elif method == "tools/list":
        reply(msg_id, {"tools": [TOOL]})
    elif method == "tools/call":
        params = msg.get("params") or {}
        args = params.get("arguments") or {}
        text = args.get("text")
        if not isinstance(text, str):
            error(msg_id, -32602, "missing text")
            continue
        reply(msg_id, {
            "content": [{"type": "text", "text": "ECHO-PROBE: " + text}],
            "isError": False,
        })
    else:
        error(msg_id, -32601, "method not found: " + str(method))
