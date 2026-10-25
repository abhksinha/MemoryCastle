#!/bin/sh
# Upgrade check: a store written by older releases must open with this build,
# from several processes at once, as at a session start (sync, serve, status
# and the hooks start together). 0.11.51 broke exactly here: an unlocked
# migration ran twice and left v29 without its column.
#   scripts/upgrade_check.sh target/release/memcastle [old tags...]
# Old tags default to the latest release. Needs curl and the release assets
# for this platform (macOS arm64, Linux x86_64).
set -eu
NEW=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
shift
REPO=abhksinha/MemoryCastle
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  *) echo "no release binaries for this platform; skipped"; exit 0 ;;
esac
TAGS="$*"
if [ -z "$TAGS" ]; then
  TAGS=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
fi
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
# A small checkout with code and a note, so every table the migrations touch has rows.
REPO_DIR="$WORK/repo"
mkdir -p "$REPO_DIR/src" "$REPO_DIR/memorycastle/journal"
cat > "$REPO_DIR/src/lib.rs" <<'EOF'
/// Encodes a frame.
pub struct Codec;
impl Codec {
    pub fn encode(&self) -> u8 { helper() }
}
fn helper() -> u8 { 1 }
EOF
cat > "$REPO_DIR/memorycastle/journal/2026-01-01-codec.md" <<'EOF'
# Codec encodes one frame at a time
Files: `src/lib.rs`

## Decision
`encode` calls `helper` once per frame because frames are small.
EOF
git -C "$REPO_DIR" init -q
git -C "$REPO_DIR" -c user.name=t -c user.email=t@t add -A
git -C "$REPO_DIR" -c user.name=t -c user.email=t@t commit -qm init
for TAG in $TAGS; do
  OLD="$WORK/old-$TAG"
  mkdir -p "$OLD"
  curl -fsSL "https://github.com/$REPO/releases/download/$TAG/memorycastle-$TAG-$TARGET.tar.gz" | tar xz -C "$OLD"
  HOME_DIR="$WORK/home-$TAG"
  export MEMORYCASTLE_HOME="$HOME_DIR" MEMORYCASTLE_NO_LEDGER=1
  (cd "$REPO_DIR" && "$OLD/memorycastle" save memorycastle/journal/2026-01-01-codec.md --keyword >/dev/null 2>&1 || "$OLD/memorycastle" save memorycastle/journal/2026-01-01-codec.md >/dev/null)
  (cd "$REPO_DIR" && "$OLD/memorycastle" graph --code >/dev/null)
  # Eight processes open the old store at once.
  i=0
  while [ $i -lt 8 ]; do
    (cd "$REPO_DIR" && "$NEW" status >/dev/null 2>"$WORK/log-$TAG-$i" || echo "exit $?" >>"$WORK/fail-$TAG") &
    i=$((i + 1))
  done
  wait
  if [ -s "$WORK/fail-$TAG" ]; then
    echo "upgrade from $TAG: a process failed"; cat "$WORK"/log-"$TAG"-*; exit 1
  fi
  OUT=$(cd "$REPO_DIR" && "$NEW" sync 2>&1 && "$NEW" links Codec::encode 2>&1 && "$NEW" search "why one frame at a time" --keyword 2>&1)
  echo "$OUT" | grep -q "fn in Codec" || { echo "upgrade from $TAG: links lost the owner"; echo "$OUT"; exit 1; }
  echo "$OUT" | grep -q "2026-01-01-codec.md" || { echo "upgrade from $TAG: search lost the note"; echo "$OUT"; exit 1; }
  echo "upgrade from $TAG: ok"
done
