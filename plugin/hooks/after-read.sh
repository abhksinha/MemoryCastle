#!/bin/sh
# After a Read or a Bash file read: report path and bytes to memorycastle.
# Prints nothing, so it costs no tokens. Silent without memorycastle or python3.
command -v memorycastle >/dev/null 2>&1 || exit 0
command -v python3 >/dev/null 2>&1 || exit 0
python3 -I "$(dirname "$0")/after-read.py" "$CLAUDE_PROJECT_DIR"
exit 0
