"""Before a Bash command that reads a whole file (`cat path`, or a `sed -n`
range over SPAN_LIMIT lines): if a lookup already printed a line range for
that file and nothing has read it, let memorycastle point at the range and
stop this call (exit 2, once per span). Silent otherwise."""
import json, os, re, subprocess, sys

SPAN_LIMIT = 150
try:
    h = json.load(sys.stdin)
except Exception:
    sys.exit(0)
cmd = (h.get("tool_input") or {}).get("command", "") or ""
path = None
m = re.match(r"\s*cat\s+(?:-[a-zA-Z]+\s+)*([~./\w-]+\.[A-Za-z0-9]+)\s*$", cmd)
if m:
    path = m.group(1)
else:
    r = re.search(r"sed\s+-n\s+['\"]?(\d+),(\d+)p['\"]?\s+([~./\w-]+\.[A-Za-z0-9]+)", cmd)
    if r and int(r.group(2)) - int(r.group(1)) >= SPAN_LIMIT:
        path = r.group(3)
if not path:
    sys.exit(0)
cwd = h.get("cwd") or os.getcwd()
path = os.path.expanduser(path)
if not os.path.isabs(path):
    path = os.path.join(cwd, path)
if not os.path.isfile(path):
    sys.exit(0)
p = subprocess.run(["memorycastle", "guard", path], cwd=os.path.dirname(path), capture_output=True, text=True)
if p.returncode == 2:
    sys.stderr.write(p.stderr)
    sys.exit(2)
sys.exit(0)
