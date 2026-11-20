#!/bin/sh
# Install cas-lean (x86_64 Linux), one of two ways:
#
#   curl -sSfL https://raw.githubusercontent.com/$REPO/dist/install.sh | sh
#   curl -sSfL https://raw.githubusercontent.com/$REPO/dist/install.sh | sh -s -- --source
#
# The first downloads the prebuilt static binary from the `dist` branch and
# checks its SHA-256; the second clones cas and flock and builds with cargo
# (Rust needed; about 5 minutes). Both install to $PREFIX/bin (default
# ~/.local/bin).
set -eu
REPO="${CAS_LEAN_REPO:-alongurny/cas}"
FLOCK_REPO="${CAS_LEAN_FLOCK_REPO:-alongurny/flock}"
PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin"
mode=binary
[ "${1:-}" = "--source" ] && mode=source

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) ;;
  *) echo "cas-lean: only x86_64 Linux is supported (this is $(uname -s)-$(uname -m))" >&2; exit 1 ;;
esac
mkdir -p "$BIN"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [ "$mode" = binary ]; then
  base="${CAS_LEAN_BASE:-https://raw.githubusercontent.com/$REPO/dist}"
  curl -sSfL "$base/cas-lean-x86_64-linux" -o "$tmp/cas-lean"
  curl -sSfL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
  want="$(awk '$2 == "cas-lean-x86_64-linux" {print $1}' "$tmp/SHA256SUMS")"
  have="$(sha256sum "$tmp/cas-lean" | awk '{print $1}')"
  if [ -z "$want" ] || [ "$want" != "$have" ]; then
    echo "cas-lean: the download's SHA-256 does not match SHA256SUMS" >&2
    exit 1
  fi
else
  command -v cargo >/dev/null || { echo "cas-lean: --source needs Rust (https://rustup.rs)" >&2; exit 1; }
  git clone -q --depth 1 "https://github.com/$REPO.git" "$tmp/cas"
  git clone -q --depth 1 -b cas "https://github.com/$FLOCK_REPO.git" "$tmp/flock"
  cargo build --release --manifest-path "$tmp/cas/lean-cli/Cargo.toml"
  cp "$tmp/cas/lean-cli/target/release/cas-lean" "$tmp/cas-lean"
fi
install -m 755 "$tmp/cas-lean" "$BIN/cas-lean"
echo "installed $BIN/cas-lean ($("$BIN/cas-lean" --version))"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "add $BIN to your PATH" ;; esac
