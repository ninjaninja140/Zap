#!/usr/bin/env bash
# verify zap builds and tests on a fresh linux environment (ubuntu/wsl).
# needs: root or sudo, internet. first run installs rustup (minimal profile).
set -e

if ! command -v cargo > /dev/null 2>&1; then
    if command -v apt-get > /dev/null 2>&1; then
        apt-get update -qq && apt-get install -y -qq curl build-essential musl-tools nodejs > /dev/null
    elif command -v dnf > /dev/null 2>&1; then
        dnf install -y curl gcc gcc-c++ make nodejs > /dev/null
    fi
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal > /dev/null 2>&1
    . "$HOME/.cargo/env"
fi

SRC="${1:-/mnt/c/Users/EdenK/Desktop/Projects/zap}"
rm -rf ~/zap-verify && cp -r "$SRC" ~/zap-verify && cd ~/zap-verify
rm -rf target

echo "### linux test ###"
cargo test --release 2>&1 | tail -3
echo "### linux build ###"
cargo build --release 2>&1 | tail -1
echo "### musl build (static binary) ###"
rustup target add x86_64-unknown-linux-musl 2>&1 | tail -1
cargo build --release --target x86_64-unknown-linux-musl 2>&1 | tail -1
MUSL_ZAP="$HOME/zap-verify/target/x86_64-unknown-linux-musl/release/zap"
[ -x "$MUSL_ZAP" ] || { echo "musl binary missing"; exit 1; }
echo "### shim smoke (pnp runtime on the musl binary) ###"
mkdir -p /tmp/zap-smoke && cd /tmp/zap-smoke
"$MUSL_ZAP" init > /dev/null
"$MUSL_ZAP" add chalk@4 2>&1 | tail -2
echo "const c = require('chalk'); console.log('unix-pnp-ok', typeof c)" > check.js
"$MUSL_ZAP" exec node check.js
echo LINUX-VERIFY-OK
