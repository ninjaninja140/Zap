#!/usr/bin/env sh

set -e

ZAP_VERSION="${1:-latest}"

os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
    Linux*) os_name="linux" ;;
    Darwin*) os_name="darwin" ;;
    *) echo "unsupported os: $os" >&2; exit 1 ;;
esac
case "$arch" in
    x86_64) arch_rust="x86_64" ;;
    arm64|aarch64) arch_rust="aarch64" ;;
    *) echo "unsupported arch: $arch" >&2; exit 1 ;;
esac

libc="gnu"

if [ "$os_name" = "linux" ]; then
    if ! command -v ldd > /dev/null 2>&1 || ! ldd --version > /dev/null 2>&1; then
        libc="musl"
    fi
fi

triple="${arch_rust}-unknown-linux-${libc}"
[ "$os_name" = "darwin" ] && triple="${arch_rust}-apple-darwin"

dest="$HOME/.zap/bin"
mkdir -p "$dest"

detect_npm_zap() {
    command -v zap > /dev/null 2>&1 || return 1
    p="$(command -v zap)"
    case "$p" in
        *node_modules*) return 0 ;;
    esac
    grep -qs "@zappkg/cli" "$p" 2>/dev/null && return 0
    return 1
}

detect_script_zap() {
    [ -f "$HOME/.zap/bin/zap" ] || [ -f "$HOME/.zap/bin/zap.exe" ]
}

if [ -n "$ZAP_INSTALLER_TEST" ]; then return 2>/dev/null || exit 0; fi

echo "  ⚡  zap installer"
echo "▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔"

if [ "$ZAP_VERSION" != "latest" ]; then
    if detect_npm_zap; then
        if command -v npm > /dev/null 2>&1; then
            echo "> removing npm install of zap"
            npm uninstall -g @zappkg/cli > /dev/null 2>&1 || {
                echo "warning: npm uninstall failed, installing anyway" >&2
            }
        else
            echo "warning: npm-managed zap detected but npm is unavailable, installing anyway" >&2
        fi
    elif detect_script_zap; then
        echo "> removing previous script install"
        rm -f "$HOME/.zap/bin/zap" "$HOME/.zap/bin/zap.exe"
    fi
fi

url="https://github.com/ninjaninja140/zap/releases/download/v$ZAP_VERSION/zap-$triple.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "> downloading $url"
if ! curl -# -fL --max-time 60 -o "$tmp/zap.tar.gz" "$url"; then
    echo "" >&2
    echo "couldn't download zap $ZAP_VERSION for $triple from $url" >&2
    exit 1
fi

tar -xzf "$tmp/zap.tar.gz" -C "$tmp"
# the binary may be nested (zap-<triple>/zap)
bin="$(find "$tmp" -name zap -type f | head -1)"
[ -n "$bin" ] || { echo "the archive held no zap binary" >&2; exit 1; }
mv "$bin" "$dest/zap"
chmod +x "$dest/zap"

echo ""
echo "zap installed to $dest/zap"

# add to PATH
case ":$PATH:" in
    *":$dest:"*) ;;
    *)
        rc="$HOME/.profile"
        [ -n "$ZSH_VERSION" ] && rc="$HOME/.zshenv"
        if [ -f "$rc" ] && grep -qsF "$dest" "$rc"; then
            echo "~ $dest is in $rc, reopen your shell to pick it up"
        else
            printf '\n# zap (added by install-zap.sh)\nexport PATH="%s:$PATH"\n' "$dest" >> "$rc"
            echo "added $dest to PATH via $rc"
            echo "~ reopen your shell to pick it up"
        fi
        ;;
esac
