"""Before a Bash command, two guards.

Writes: a command that writes files through the shell (`sed -i`, `perl -pi`,
`> f`, `>> f`, `tee f`, a Python `open(f, 'w')` or `Path(f).write_text`,
heredocs included) gets the same edit gate as the Edit tool: each file's
first write this session gets what `links` knows about it (as context, or
stopped once with MEMORYCASTLE_GATE=block).
Without this, the gate is one heredoc away from never firing. Best effort:
a write the parser misses is still looked up after the command, by
`memcastle changed` in after-bash.sh.

Reads: a command that reads a whole file (`cat path`, or a `sed -n` range
over SPAN_LIMIT lines) of a file a lookup already printed a line range for,
when nothing has read it: point at the range and stop this call (exit 2,
once per span). Silent otherwise."""
import json, os, re, shlex, subprocess, sys

SPAN_LIMIT = 150
# A path worth gating: has a letter extension or a directory part, no glob.
PATHLIKE = re.compile(r"^[~\w./-]*(?:/[\w.-]+|\.[A-Za-z]\w*)$")
try:
    h = json.load(sys.stdin)
except Exception:
    sys.exit(0)
# This event's session, for every memorycastle call below.
if h.get("session_id"):
    os.environ["MEMORYCASTLE_SESSION"] = h["session_id"]
cmd = (h.get("tool_input") or {}).get("command", "") or ""
cwd = h.get("cwd") or os.getcwd()
# `cd /elsewhere && cat > f`, or a `cd` after other commands: relative paths
# are under the first directory the command changes to.
lead = re.search(r"(?:^|&&|;|\|\||\n)\s*cd\s+(\"[^\"]+\"|'[^']+'|[^\s;&|]+)\s*(?:&&|;|\n)", cmd)
if lead:
    cwd = os.path.join(cwd, os.path.expanduser(lead.group(1).strip("'\"")))


def absolute(p):
    p = os.path.expanduser(p)
    return p if os.path.isabs(p) else os.path.join(cwd, p)


def split_heredocs(text):
    """The command without heredoc bodies, and the bodies: a `>` inside a
    Python heredoc is a comparison, not a redirect."""
    shell, bodies = [], []
    lines = text.split("\n")
    i = 0
    while i < len(lines):
        line = lines[i]
        shell.append(line)
        tags = re.findall(r"<<-?\s*['\"]?(\w+)['\"]?", line)
        i += 1
        for tag in tags:
            body = []
            while i < len(lines) and lines[i].strip() != tag:
                body.append(lines[i])
                i += 1
            i += 1
            bodies.append("\n".join(body))
    return "\n".join(shell), "\n".join(bodies)


def written(text):
    shell, bodies = split_heredocs(text)
    out = []
    # Redirects: `> f`, `>> f`, `>| f`; not `2>&1`, `&>`, `/dev/...`.
    for m in re.finditer(r"(?<![<>&\d])>{1,2}\|?\s*([^\s;&|<>()]+)", shell):
        out.append(m.group(1).strip("'\""))
    for seg in re.split(r"&&|\|\||[;|\n]", shell):
        try:
            words = shlex.split(seg, comments=True)
        except ValueError:
            words = seg.split()
        if not words:
            continue
        prog = os.path.basename(words[0])
        args = words[1:]
        if prog == "sed" and any(re.match(r"-[A-Za-z]*i|--in-place", a) for a in args):
            out += [a for a in args if not a.startswith("-") and os.path.isfile(absolute(a))]
        elif prog == "perl" and any(a.startswith("-") and "i" in a for a in args):
            out += [a for a in args if not a.startswith("-") and os.path.isfile(absolute(a))]
        elif prog == "tee":
            out += [a for a in args if not a.startswith("-")]
    # Python, in a heredoc or `python3 -c`.
    for src in (bodies, shell):
        out += re.findall(r"open\(\s*[rbf]?['\"]([^'\"]+)['\"]\s*,\s*[rbf]?['\"][^'\"]*[wax]", src)
        out += re.findall(r"Path\(\s*['\"]([^'\"]+)['\"]\s*\)\.write_(?:text|bytes)", src)
        # `p = 'src/x.rs'` then `open(p, 'w')`: the string the name last held
        # before that call.
        sets = [(m.start(), m.group(1), m.group(2)) for m in re.finditer(r"\b(\w+)\s*=\s*['\"]([^'\"\n]+)['\"]", src)]
        for m in re.finditer(r"open\(\s*(\w+)\s*,\s*['\"][^'\"]*[wax]", src):
            held = [v for at, n, v in sets if n == m.group(1) and at < m.start()]
            if held:
                out.append(held[-1])
    seen = []
    for p in out:
        if p.startswith("/dev/") or not PATHLIKE.match(p) or p in seen:
            continue
        seen.append(p)
    return seen


def gate(paths):
    blocks, context = [], []
    for p in paths:
        a = absolute(p)
        d = os.path.dirname(a)
        while d and not os.path.isdir(d):
            d = os.path.dirname(d)
        if not d:
            continue
        r = subprocess.run(["memorycastle", "gate", a], cwd=d, capture_output=True, text=True)
        if r.returncode == 2 and r.stderr.strip():
            blocks.append(r.stderr.strip())
        elif r.stdout.strip():
            context.append(r.stdout.strip())
    if blocks:
        sys.stderr.write("\n".join(blocks + context) + "\nThe command writes these files; it passes on the retry.\n")
        sys.exit(2)
    if context:
        print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": "\n".join(context)}}))
        sys.exit(0)


gate(written(cmd))

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
path = absolute(path)
if not os.path.isfile(path):
    sys.exit(0)
p = subprocess.run(["memorycastle", "guard", path], cwd=os.path.dirname(path), capture_output=True, text=True)
if p.returncode == 2:
    sys.stderr.write(p.stderr)
    sys.exit(2)
sys.exit(0)
