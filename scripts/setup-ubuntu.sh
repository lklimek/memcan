#!/usr/bin/env bash
# Setup script for Ubuntu build dependencies.
# Installs system packages and protoc from GitHub releases.
set -euo pipefail

PROTOC_VERSION="${PROTOC_VERSION:-29.4}"
INSTALL_PREFIX="${INSTALL_PREFIX:-/usr/local}"

arch="$(uname -m)"
case "${arch}" in
  x86_64)  PROTOC_ARCH="x86_64" ;;
  aarch64) PROTOC_ARCH="aarch_64" ;;
  *)
    echo "Unsupported architecture: ${arch}" >&2
    exit 1
    ;;
esac

echo "==> Installing system packages"
sudo apt-get update -q
sudo apt-get install -y -q \
  build-essential \
  libssl-dev \
  pkg-config \
  unzip \
  curl

echo "==> Installing protoc ${PROTOC_VERSION} (${PROTOC_ARCH})"
PROTOC_ZIP="protoc-${PROTOC_VERSION}-linux-${PROTOC_ARCH}.zip"
PROTOC_URL="https://github.com/protocolbuffers/protobuf/releases/download/v${PROTOC_VERSION}/${PROTOC_ZIP}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

curl -fsSL "${PROTOC_URL}" -o "${TMP_DIR}/${PROTOC_ZIP}"
unzip -q "${TMP_DIR}/${PROTOC_ZIP}" -d "${TMP_DIR}/protoc"
sudo install -m 0755 "${TMP_DIR}/protoc/bin/protoc" "${INSTALL_PREFIX}/bin/protoc"
sudo cp -r "${TMP_DIR}/protoc/include/." "${INSTALL_PREFIX}/include/"

echo "==> protoc installed: $(protoc --version)"
