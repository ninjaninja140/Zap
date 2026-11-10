#!/usr/bin/env bash
# verify zap builds and tests on a fresh linux environment (ubuntu/wsl).
# needs: root or sudo, internet. first run installs rustup (minimal profile).
set -e

if ! command -v cargo > /dev/null 2>&1; then
    if command -v apt-get > /dev/null 2>&1; then
        apt-get update -qq && apt-get install -y -qq curl build-essential nodejs > /dev/null
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
echo "### shim smoke (pnp runtime on real unix) ###"
mkdir -p /tmp/zap-smoke && cd /tmp/zap-smoke
~/zap-verify/target/release/zap init > /dev/null
~/zap-verify/target/release/zap add chalk@4 2>&1 | tail -2
echo "const c = require('chalk'); console.log('unix-pnp-ok', typeof c)" > check.js
~/zap-verify/target/release/zap exec node check.js
echo LINUX-VERIFY-OK
