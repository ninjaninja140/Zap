#!/usr/bin/env sh
# zap quick test build (mac + linux)
# unix verification (fresh toolchain + pnp smoke install), scripts/verify-linux.sh
set -e

os="$(uname -s)"

case "$1" in
  --release) cargo build --release; bin=target/release/zap ;;
  *)         cargo build;           bin=target/debug/zap ;;
esac

cargo test

# smoke: binary must run (exit 0); output silenced so it doesn't print a footer
"$bin" --version > /dev/null 2>&1

echo
echo "built: $bin ($os)"
