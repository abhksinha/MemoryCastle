#!/bin/sh
# Before a Grep, or a Bash grep/rg: a search for one name the code index
# defines is answered once per session with `links <name>` (exit 2, the retry
# passes). The logic is the binary's, so it changes with the binary.
command -v memorycastle >/dev/null 2>&1 || exit 0
exec memcastle gate --grep-hook
