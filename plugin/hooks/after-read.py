"""After a Read or a Bash file read: tell memorycastle what was read and how
many bytes came back. Prints nothing, so it costs no tokens."""
import json, os, re, subprocess, sys

try:
    h = json.load(sys.stdin)
except Exception:
    sys.exit(0)
# This event's session, for every memorycastle call below.
if h.get("session_id"):
    os.environ["MEMORYCASTLE_SESSION"] = h["session_id"]
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
    # Every file argument of every cat/sed/head/tail in the command, not
    # only the first: `cat a b` and `head x; cat y` read each of them.
    # A redirect target (`cat > note.md <<EOF`) and `sed -i` are writes.
    for seg in re.finditer(r"\b(?:cat|sed|head|tail)\b([^|;&]*)", cmd):
        args = seg.group(1)
        if re.search(r"(?:^|\s)-i\b", args):
            continue
        args = re.sub(r"\d?>>?\s*\S+", " ", args)
        for p in re.findall(r"\s((?:[~./\w-]+/)?[\w.-]+\.[A-Za-z0-9]+)(?=\s|$)", args):
            if p not in paths:
                paths.append(p)
    if len(paths) == 1:
        r = re.search(r"sed\s+-n\s+['\"]?(\d+),(\d+)p", cmd)
        if r:
            lines = (int(r.group(1)), int(r.group(2)))
if not paths or nbytes == 0:
    sys.exit(0)
# One output for several files: split the bytes so the ledger does not
# count the whole output once per file.
nbytes = max(1, nbytes // len(paths))
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
