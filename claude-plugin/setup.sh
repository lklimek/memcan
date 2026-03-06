#!/usr/bin/env bash
# MindOJO plugin setup — downloads the latest release binary for the current platform.
#
# Called by Claude Code when the plugin is installed or updated.
# Puts the binary in claude-plugin/bin/

set -euo pipefail

REPO="lklimek/mindojo"
BIN_DIR="$(cd "$(dirname "$0")" && pwd)/bin"
mkdir -p "$BIN_DIR"

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

# Try gh CLI first (usually available in Claude Code environments)
if command -v gh &>/dev/null; then
    TAG=$(gh release view --repo "$REPO" --json tagName -q '.tagName' 2>/dev/null || true)
    if [ -n "$TAG" ]; then
        echo "Latest release: $TAG"
        echo "Downloading $ASSET..."
        gh release download "$TAG" --repo "$REPO" --pattern "$ASSET" --dir /tmp --clobber
        tar xzf "/tmp/$ASSET" -C "$BIN_DIR"
        rm -f "/tmp/$ASSET"
        chmod +x "$BIN_DIR"/mindojo-*
        echo "Installed MindOJO binaries to $BIN_DIR"
        ls -la "$BIN_DIR"/mindojo-*
        exit 0
    fi
fi

# Fallback: curl + GitHub API
LATEST_URL="https://api.github.com/repos/$REPO/releases/latest"
TAG=$(curl -sf "$LATEST_URL" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')

if [ -z "$TAG" ]; then
    echo "Error: Could not determine latest release."
    echo "Build from source: cargo build --release (from repo root)"
    exit 1
fi

DOWNLOAD_URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"
echo "Latest release: $TAG"
echo "Downloading $DOWNLOAD_URL..."

curl -fSL "$DOWNLOAD_URL" -o "/tmp/$ASSET"
tar xzf "/tmp/$ASSET" -C "$BIN_DIR"
rm -f "/tmp/$ASSET"
chmod +x "$BIN_DIR"/mindojo-*

echo "Installed MindOJO binaries to $BIN_DIR"
ls -la "$BIN_DIR"/mindojo-*
