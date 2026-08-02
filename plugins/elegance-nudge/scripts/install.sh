#!/usr/bin/env bash
# SessionStart: make sure a matching `elegance` binary exists, then get
# out of the way.
#
# The plugin cannot ship the binary — half of it is C compiled per
# architecture, and there are eight — so it fetches the one this machine
# needs from the latest release. Three properties that are not
# negotiable for something a hook will execute:
#
#   VERIFIED. Every release publishes a `.sha256` beside its binary and
#     this refuses to install anything that does not match it. A
#     download that fails the check is deleted, not run.
#   VISIBLE. It says what it fetched and from where. A plugin that
#     silently puts an executable on your disk has earned no trust.
#   IDEMPOTENT. It asks what the latest release's checksum IS — a file
#     of about a hundred bytes — and stops there when the installed
#     binary already hashes to it. So the steady state costs one tiny
#     request and no download.
#
# Keying on the checksum rather than on a version is deliberate, and
# the first draft got it wrong: it parsed the tag out of the redirect
# from /releases/latest, which GitHub answers with /releases and no tag
# in it, so the whole thing silently did nothing on a machine with no
# binary. The checksum needs no API call, no rate limit and no parsing,
# it is fetched anyway to verify the download, and it is the exact
# identity of the thing on disk rather than a label attached to it.
#
# An existing `elegance` on PATH always wins: someone who built from
# source meant it.

set -euo pipefail

REPO="GrigoryEvko/elegance"
DATA="${CLAUDE_PLUGIN_DATA:-$HOME/.claude/elegance-nudge}"
BIN_DIR="$DATA/bin"
BIN="$BIN_DIR/elegance"
STAMP="$DATA/installed-sha256"

# Someone else's elegance, deliberately installed, is the one to use.
for candidate in "$(command -v elegance 2>/dev/null || true)" "$HOME/.cargo/bin/elegance" "$HOME/.local/bin/elegance"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then
        exit 0
    fi
done

command -v curl >/dev/null 2>&1 || exit 0

# Which of the eight this machine can run. musl on Linux on purpose:
# static, so it does not care what libc the host or container has.
os=$(uname -s 2>/dev/null || echo unknown)
arch=$(uname -m 2>/dev/null || echo unknown)
case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) arch=unsupported ;;
esac
case "$os" in
    Linux) asset="elegance-$arch-linux-musl" ;;
    Darwin) asset="elegance-$arch-macos" ;;
    MINGW* | MSYS* | CYGWIN* | Windows_NT) asset="elegance-$arch-windows.exe" ;;
    *) asset="" ;;
esac
if [ -z "$asset" ] || [ "$arch" = "unsupported" ]; then
    echo "elegance-nudge: no released binary for $os/$arch — cargo install --git https://github.com/$REPO"
    exit 0
fi

# Whichever tool this platform spells it with.
sha_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}
[ -n "$(sha_of /dev/null)" ] || exit 0   # nothing to verify with: install nothing

# `/releases/latest/download/` always serves the newest release's asset,
# so the checksum of the newest build is one small request away.
BASE="https://github.com/$REPO/releases/latest/download"
want=$(curl -fsSL "$BASE/$asset.sha256" 2>/dev/null | cut -d' ' -f1)
if [ -z "$want" ]; then
    exit 0   # offline, or no release yet. Whatever is installed stays.
fi
if [ -x "$BIN" ] && [ "$(sha_of "$BIN")" = "$want" ]; then
    exit 0   # already the current build: the steady state, and it is cheap
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
if ! curl -fsSL -o "$TMP/bin" "$BASE/$asset"; then
    echo "elegance-nudge: could not download $asset"
    exit 0
fi
got=$(sha_of "$TMP/bin")
if [ "$want" != "$got" ]; then
    echo "elegance-nudge: checksum mismatch for $asset — not installed"
    exit 0
fi

mkdir -p "$BIN_DIR"
mv "$TMP/bin" "$BIN"
chmod 755 "$BIN"
printf '%s' "$want" > "$STAMP"
# Ask the binary what it is, and believe it only if it answers the
# question. Releases before 0.1.1 have no --version, so they scan the
# working directory instead and reply "no supported source files
# found" — which would have gone into this message as if it were a
# version string.
version=$("$BIN" --version 2>/dev/null | head -1 || true)
case "$version" in
    elegance\ *) ;;
    *) version="" ;;
esac
echo "elegance-nudge: installed $asset${version:+ ($version)} in $BIN_DIR, sha256 verified"
