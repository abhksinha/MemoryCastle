#!/usr/bin/env bash
# Publishes this checkout to a new GitHub repository and tags the first release.
# Usage: ./scripts/publish.sh <github-user>/<repo> [--private]
# Needs: git; gh (GitHub CLI) if the repository does not exist yet.
set -euo pipefail

cd "$(dirname "$0")/.."
[ $# -ge 1 ] || { sed -n 2,4p "$0"; exit 2; }
SLUG="$1"; VIS="--public"; [ "${2:-}" = "--private" ] && VIS="--private"
VERSION="v$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"

[ -z "$(git status --porcelain)" ] || { echo "working tree not clean" >&2; exit 1; }
cargo test --release --quiet
cargo clippy --all-targets --quiet -- -D warnings

for f in Cargo.toml README.md CLAUDE.md; do
  sed -i '' "s#github.com/<you>/memorycastle#github.com/$SLUG#g" "$f" 2>/dev/null || true
done
if [ -n "$(git status --porcelain)" ]; then
  git add -A && git commit -q -m "Point links at github.com/$SLUG"
fi

if ! git remote get-url origin >/dev/null 2>&1; then
  if command -v gh >/dev/null 2>&1; then
    gh repo create "$SLUG" $VIS --source=. --remote=origin \
      --description "$(grep -m1 '^description' Cargo.toml | cut -d'"' -f2)"
  else
    echo "gh not installed. Create https://github.com/$SLUG empty in the browser, then:" >&2
    echo "  git remote add origin git@github.com:$SLUG.git && $0 $SLUG" >&2
    exit 1
  fi
fi

git push -u origin HEAD
git tag -a "$VERSION" -m "memorycastle $VERSION" 2>/dev/null || true
git push origin "$VERSION"
echo "published: https://github.com/$SLUG  tag $VERSION"
echo "CI builds and tests on macOS and Linux; the release workflow attaches binaries to the tag."
