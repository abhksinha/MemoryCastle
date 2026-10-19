#!/bin/sh
# One-line installer:  curl -fsSL https://raw.githubusercontent.com/abhksinha/MemoryCastle/main/scripts/get.sh | sh
# Downloads the release binaries for this machine (macOS arm64, Linux x86_64)
# into ~/.local/bin, verified by SHA-256; builds from source with cargo when
# there is no binary for the platform. Then: cd your-repo && memcastle install
set -eu
REPO=abhksinha/MemoryCastle
BIN_DIR="${MEMORYCASTLE_BIN:-$HOME/.local/bin}"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  *) TARGET="" ;;
esac
TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
if [ -n "$TARGET" ] && [ -n "$TAG" ]; then
  URL="https://github.com/$REPO/releases/download/$TAG/memorycastle-$TAG-$TARGET"
  if curl -fsSL "$URL.tar.gz" -o "$TMP/mc.tar.gz" && curl -fsSL "$URL.sha256" -o "$TMP/mc.sha256"; then
    (cd "$TMP" && sed "s|memorycastle-.*tar.gz|mc.tar.gz|" mc.sha256 > sum && shasum -a 256 -c sum >/dev/null) || { echo "checksum mismatch" >&2; exit 1; }
    mkdir -p "$BIN_DIR"
    tar xzf "$TMP/mc.tar.gz" -C "$BIN_DIR"
    [ -x "$BIN_DIR/memcastle" ] || ln -sf "$BIN_DIR/memorycastle" "$BIN_DIR/memcastle"
    echo "installed memorycastle $TAG to $BIN_DIR"
    case ":$PATH:" in *":$BIN_DIR:"*) ;; *) echo "add $BIN_DIR to PATH";; esac
    echo "next: cd your-repo && memcastle install"
    exit 0
  fi
fi
echo "no binary for $(uname -s)-$(uname -m); building from source (needs cargo, about 2 minutes)"
command -v cargo >/dev/null 2>&1 || { echo "install Rust first: https://rustup.rs" >&2; exit 1; }
git clone -q --depth 1 "https://github.com/$REPO" "$TMP/src"
cargo install --path "$TMP/src" --locked
echo "next: cd your-repo && memcastle install"
