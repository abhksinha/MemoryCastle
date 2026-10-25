#!/usr/bin/env bash
# Builds memorycastle from source and installs it to ~/.cargo/bin.
# Usage: ./scripts/install.sh [--keyword-only] [--prefetch-model]
set -euo pipefail

cd "$(dirname "$0")/.."
KEYWORD_ONLY=0
PREFETCH=0
for a in "$@"; do
  case "$a" in
    --keyword-only) KEYWORD_ONLY=1 ;;
    --prefetch-model) PREFETCH=1 ;;
    -h|--help) sed -n 2,3p "$0"; exit 0 ;;
    *) echo "unknown flag: $a" >&2; exit 2 ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust toolchain not found."
  read -r -p "Install it with rustup now? [y/N] " yn
  if [ "${yn:-n}" = "y" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  else
    echo "See https://rustup.rs and rerun." >&2
    exit 1
  fi
fi

echo "== building (release)"
if [ "$KEYWORD_ONLY" = 1 ]; then
  cargo install --path . --no-default-features --locked
else
  cargo install --path . --locked
fi

BIN="$HOME/.cargo/bin/memorycastle"
echo "== installed: $BIN ($(du -h "$BIN" | cut -f1))"
case ":$PATH:" in
  *":$HOME/.cargo/bin:"*) ;;
  *) echo "note: add \$HOME/.cargo/bin to PATH" ;;
esac

if [ "$PREFETCH" = 1 ] && [ "$KEYWORD_ONLY" = 0 ]; then
  echo "== prefetching ONNX Runtime and the int8 model (about 350 MB, verified by SHA-256)"
  MEMORYCASTLE_HOME="${MEMORYCASTLE_HOME:-$HOME/.local/share/memorycastle}" \
    "$BIN" --quiet search "warm up" --meaning >/dev/null 2>&1 || true
fi

cat <<'TXT'

Next, inside a git repository:
  memcastle install                     # wires every agent found on this machine
  memcastle search "what did we decide about X"
TXT
