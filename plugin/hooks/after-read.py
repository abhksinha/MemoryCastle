"""After a Read or a Bash file read: tell memorycastle what was read and how
many bytes came back. Prints nothing, so it costs no tokens."""
import json, os, re, subprocess, sys

try:
    h = json.load(sys.stdin)
except Exception:
    sys.exit(0)
tool = h.get("tool_name", "")
inp = h.get("tool_input", {}) or {}
resp = h.get("tool_response", "")
if isinstance(resp, dict):
    text = resp.get("stdout") or resp.get("content") or (resp.get("file") or {}).get("content", "") or ""
    if not isinstance(text, str):
        text = json.dumps(text)
else:
    text = str(resp)
nbytes = len(text.encode("utf-8", "ignore"))
paths = []
lines = None
if tool == "Read":
    p = inp.get("file_path")
    if p:
        paths.append(p)
        off = inp.get("offset")
        lim = inp.get("limit")
        if off or lim:
            a = int(off or 1)
            lines = (a, a + int(lim or 2000) - 1)
elif tool == "Bash":
    cmd = inp.get("command", "")
    m = re.search(r"\b(?:cat|sed|head|tail)\b[^|;&]*?\s((?:[~./\w-]+/)?[\w.-]+\.[A-Za-z0-9]+)\b", cmd)
    if m:
        paths.append(m.group(1))
        r = re.search(r"sed\s+-n\s+['\"]?(\d+),(\d+)p", cmd)
        if r:
            lines = (int(r.group(1)), int(r.group(2)))
if not paths or nbytes == 0:
    sys.exit(0)
cwd = h.get("cwd") or (sys.argv[1] if len(sys.argv) > 1 else "") or os.getcwd()
for p in paths:
    p = os.path.expanduser(p)
    if not os.path.isabs(p):
        p = os.path.join(cwd, p)
    if not os.path.isfile(p):
        continue
    args = ["memorycastle", "read", p, "--bytes", str(nbytes)]
    if lines:
        args += ["--lines", f"{lines[0]}-{lines[1]}"]
    # Run from the file's own directory: memorycastle resolves the project
    # from the cwd, and the file may be in another repo than the session's.
    subprocess.run(args, cwd=os.path.dirname(p) or cwd,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
