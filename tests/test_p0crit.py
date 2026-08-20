"""P0-CRIT regression test: verify herdr_reap's cwd gate is workspace-scoped."""
import json, urllib.request

U = "http://127.0.0.1:8772/mcp"
T = "Bearer testtoken"  # placeholder; real token lives in launchd plist, not in source
H = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
     "MCP-Protocol-Version": "2025-11-25", "Authorization": T}

def rpc(p, sid=None):
    h = dict(H)
    if sid: h["Mcp-Session-Id"] = sid
    r = urllib.request.Request(U, data=json.dumps(p).encode(), headers=h)
    with urllib.request.urlopen(r, timeout=15) as x:
        b = x.read().decode()
        s = x.headers.get("Mcp-Session-Id")
        if "data:" in b: b = b.split("data:")[-1]
        return json.loads(b) if b.strip() else {}, s

_, sid = rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize",
              "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                         "clientInfo": {"name": "p0crit-regression", "version": "1"}}})
rpc({"jsonrpc": "2.0", "method": "notifications/initialized"}, sid=sid)

# Get all workspaces and their panes
r, _ = rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
             "params": {"name": "herdr_inspect", "arguments": {}}}, sid=sid)
j = json.loads(r["result"]["content"][0]["text"])
agents = j.get("agents", [])
all_ws = set(a["workspace"] for a in agents if a.get("workspace"))
print(f"total workspaces with agents: {len(all_ws)}: {sorted(all_ws)}")

# Create session, get its workspace
r, _ = rpc({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
             "params": {"name": "herdr_session",
                        "arguments": {"label": "p0crit-regression-test",
                                      "cwd": "/tmp/p0crit-nonexistent",
                                      "resume": False}}}, sid=sid)
j = json.loads(r["result"]["content"][0]["text"])
sess_ws = j.get("workspace_id", "")
print(f"session created: ws={sess_ws} cwd={j.get('cwd')}")

# Reap with close_workspace=true (should trigger cwd_mismatch since /tmp doesn't match)
r, _ = rpc({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
             "params": {"name": "herdr_reap",
                        "arguments": {"session": "p0crit-regression-test",
                                      "close_workspace": True}}}, sid=sid)
j = json.loads(r["result"]["content"][0]["text"])

if j.get("reason") == "cwd_mismatch":
    safe = j.get("safe_panes", [])
    mism = [m.get("pane") for m in j.get("mismatched_panes", [])]
    all_panes = set(safe) | set(mism)
    ws_prefixes = set(p.split(":")[0] for p in all_panes if p and ":" in p)
    print(f"cwd_mismatch triggered: safe={safe} mismatched={mism}")
    print(f"workspace prefixes: {ws_prefixes}")
    if ws_prefixes <= {sess_ws}:
        print("PASS: only session workspace panes listed")
    else:
        print(f"FAIL: cross-workspace panes detected! Expected only {sess_ws}")
elif j.get("ok"):
    print(f"PASS (empty workspace, closed cleanly): closed={j.get('closed')}")
    # New session workspace has only its own root pane, cwd may match if
    # the workspace creation sets cwd correctly. Either way no cross-ws leak.
    # Verify workspace was closed by checking it's gone
    r2, _ = rpc({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
                  "params": {"name": "herdr_inspect", "arguments": {}}}, sid=sid)
    j2 = json.loads(r2["result"]["content"][0]["text"])
    remaining_ws = [w["id"] for w in j2.get("workspaces", []) if w["id"] == sess_ws]
    if not remaining_ws:
        print(f"PASS: workspace {sess_ws} properly closed")
    else:
        print(f"WARN: workspace {sess_ws} still exists after close")
else:
    print(f"UNEXPECTED: {json.dumps(j, ensure_ascii=False)[:300]}")
