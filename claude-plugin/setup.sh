#!/usr/bin/env bash
# MindOJO plugin setup — downloads the latest release binary for the current platform.
#
# Called by Claude Code when the plugin is installed or updated.
# Puts the binary in claude-plugin/bin/
#
# Set MINDOJO_BUILD_FROM_SOURCE=1 to build from the local repo instead of downloading.

set -euo pipefail

BIN_DIR="$(cd "$(dirname "$0")" && pwd)/bin"
mkdir -p "$BIN_DIR"

# Build from source if requested
if [ -n "${MINDOJO_BUILD_FROM_SOURCE:-}" ]; then
    REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
    echo "Building MindOJO from source ($REPO_ROOT)..."
    cargo build --release --workspace --manifest-path "$REPO_ROOT/Cargo.toml"
    find "$REPO_ROOT/target/release" -maxdepth 1 -name 'mindojo-*' ! -name '*.d' -type f \
        -exec cp {} "$BIN_DIR/" \;
    chmod +x "$BIN_DIR"/mindojo-*
    echo "Installed MindOJO binaries (built from source) to $BIN_DIR"
    ls -la "$BIN_DIR"/mindojo-*
    exit 0
fi

REPO="lklimek/mindojo"

# Detect platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)
        case "$ARCH" in
            x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
            aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
            *)       echo "Unsupported architecture: $ARCH"; exit 1 ;;
        esac
        ;;
    Darwin)
        case "$ARCH" in
            x86_64)  TARGET="x86_64-apple-darwin" ;;
            arm64)   TARGET="aarch64-apple-darwin" ;;
            *)       echo "Unsupported architecture: $ARCH"; exit 1 ;;
        esac
        ;;
    *)
        echo "Unsupported OS: $OS"; exit 1 ;;
esac

ASSET="mindojo-${TARGET}.tar.gz"

echo "Detecting latest release..."

LATEST_URL="https://api.github.com/repos/$REPO/releases/latest"
TAG=$(curl -sf "$LATEST_URL" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')

if [ -z "$TAG" ]; then
    echo "Error: Could not determine latest release."
    echo "Build from source: MINDOJO_BUILD_FROM_SOURCE=1 bash setup.sh"
    exit 1
fi

DOWNLOAD_URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"
echo "Latest release: $TAG"
echo "Downloading $DOWNLOAD_URL..."

curl -fSL "$DOWNLOAD_URL" -o "/tmp/$ASSET"
# Verify checksum if available
SUMS_URL="https://github.com/$REPO/releases/download/$TAG/SHA256SUMS"
if curl -fsSL "$SUMS_URL" -o /tmp/SHA256SUMS 2>/dev/null; then
    echo "Verifying checksum..."
    (cd /tmp && grep "$ASSET" SHA256SUMS | sha256sum -c --status) || {
        echo "ERROR: Checksum verification failed for $ASSET"
        rm -f "/tmp/$ASSET" "/tmp/SHA256SUMS"
        exit 1
    }
    echo "Checksum OK"
    rm -f /tmp/SHA256SUMS
else
    echo "Warning: SHA256SUMS not found, skipping integrity check"
fi
tar xzf "/tmp/$ASSET" -C "$BIN_DIR"
rm -f "/tmp/$ASSET"
chmod +x "$BIN_DIR"/mindojo-*

echo "Installed MindOJO binaries to $BIN_DIR"
ls -la "$BIN_DIR"/mindojo-*
