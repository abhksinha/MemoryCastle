#!/bin/sh
# When the agent stops: remind to save journal notes edited since their last save.
command -v memorycastle >/dev/null 2>&1 || exit 0
changed=$(memcastle status 2>/dev/null | sed -n 's/^changed on disk since saved: //p')
[ -n "$changed" ] && printf 'memorycastle: unsaved journal notes: %s. Run: memcastle save --changed\n' "$changed"
# A session that edited code and saved no note gets a draft from the ledger:
# Files: from the edits, Author: from the session; the agent fills Decision.
draft=$(memcastle draft 2>/dev/null)
[ -n "$draft" ] && printf 'memorycastle: this session edited code and saved no note. Draft at %s: set the title to the decision, fill Decision and Measured, then: memcastle save %s\n' "$draft" "$draft"
exit 0
