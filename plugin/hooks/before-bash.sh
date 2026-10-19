#!/bin/sh
# Before a Bash whole-file read: same guard as before-read.sh, for `cat` and
# long `sed -n` ranges. Silent without memorycastle or python3.
command -v memorycastle >/dev/null 2>&1 || exit 0
command -v python3 >/dev/null 2>&1 || exit 0
exec python3 -I "$(dirname "$0")/before-bash.py"
