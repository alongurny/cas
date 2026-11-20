#!/bin/sh
# Build the prebuilt binary for the `dist` branch: static, for any x86_64
# Linux with AVX2 (x86-64-v3) and carry-less multiplication.
set -eu
cd "$(dirname "$0")"
RUSTFLAGS="-C target-cpu=x86-64-v3 -C target-feature=+pclmulqdq,+aes,+crt-static" \
  cargo build --release --target x86_64-unknown-linux-gnu --target-dir target/dist
mkdir -p dist
cp target/dist/x86_64-unknown-linux-gnu/release/cas-lean dist/cas-lean-x86_64-linux
cp install.sh dist/install.sh
(cd dist && sha256sum cas-lean-x86_64-linux > SHA256SUMS && cat SHA256SUMS)
