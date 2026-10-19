#!/bin/sh
# Before the context is compacted: the why of this session's edits is about
# to be summarised away. Draft the note now so what remains is filling two
# sections; the post-compaction session-start hook says where it is.
command -v memorycastle >/dev/null 2>&1 || exit 0
draft=$(memcastle draft 2>/dev/null)
[ -n "$draft" ] && printf 'memorycastle: context is about to be compacted and this session edited code without a note. Draft at %s: fill Decision and Measured now, then: memcastle save %s\n' "$draft" "$draft"
exit 0
