#!/usr/bin/env bash
set -euo pipefail

REPO="lklimek/memcan"
BIN_NAME="memcan"
DEFAULT_INSTALL_DIR="${HOME}/.local/bin"
DEFAULT_SERVER_DIR="${HOME}/.config/memcan/server"
CLI_ENV_DIR="${HOME}/.config/memcan"

# --- Colors (degrade if not a tty) ---
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    CYAN='\033[0;36m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    RED='' GREEN='' YELLOW='' CYAN='' BOLD='' RESET=''
fi

info()  { printf "${CYAN}info:${RESET} %s\n" "$*"; }
ok()    { printf "${GREEN}  ok:${RESET} %s\n" "$*"; }
warn()  { printf "${YELLOW}warn:${RESET} %s\n" "$*" >&2; }
die()   { printf "${RED}error:${RESET} %s\n" "$*" >&2; exit 1; }

# --- Usage ---
usage() {
    cat <<EOF
${BOLD}setup.sh${RESET} — install memcan CLI and set up server via Docker Compose

Usage:
    bash setup.sh [OPTIONS]
    curl -fsSL https://raw.githubusercontent.com/${REPO}/main/setup.sh | bash

Options:
    --version VERSION   Install a specific version (e.g. v0.29.0). Default: latest.
    --install-dir DIR   CLI binary location (default: ${DEFAULT_INSTALL_DIR}).
    --server-dir DIR    Docker Compose config location (default: ${DEFAULT_SERVER_DIR}).
    --cli-only          Only install CLI binary, skip server setup.
    --help              Show this help message.

Examples:
    bash setup.sh
    bash setup.sh --version v0.29.0
    bash setup.sh --cli-only
    bash setup.sh --server-dir /opt/memcan
EOF
    exit 0
}

# --- Parse args ---
VERSION=""
INSTALL_DIR="${DEFAULT_INSTALL_DIR}"
SERVER_DIR="${DEFAULT_SERVER_DIR}"
CLI_ONLY=false

while [ $# -gt 0 ]; do
    case "$1" in
        --help) usage ;;
        --version)
            [ $# -ge 2 ] || die "--version requires an argument"
            VERSION="$2"; shift 2 ;;
        --install-dir)
            [ $# -ge 2 ] || die "--install-dir requires an argument"
            INSTALL_DIR="$2"; shift 2 ;;
        --server-dir)
            [ $# -ge 2 ] || die "--server-dir requires an argument"
            SERVER_DIR="$2"; shift 2 ;;
        --cli-only)
            CLI_ONLY=true; shift ;;
        *) die "Unknown option: $1. Use --help for usage." ;;
    esac
done

# --- Detect platform ---
detect_target() {
    local os arch target
    os="$(uname -s)"
    arch="$(uname -m)"

    case "${os}" in
        Linux)
            case "${arch}" in
                x86_64)  target="x86_64-unknown-linux-musl" ;;
                aarch64) target="aarch64-unknown-linux-gnu" ;;
                *) die "Unsupported Linux architecture: ${arch}" ;;
            esac
            ;;
        Darwin)
            case "${arch}" in
                x86_64)  target="x86_64-apple-darwin" ;;
                arm64)   target="aarch64-apple-darwin" ;;
                *) die "Unsupported macOS architecture: ${arch}" ;;
            esac
            ;;
        *) die "Unsupported OS: ${os}" ;;
    esac

    printf '%s' "${target}"
}

# --- Checksum verifier ---
verify_checksum() {
    local file="$1" expected="$2"
    local actual

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "${file}" | cut -d' ' -f1)"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "${file}" | cut -d' ' -f1)"
    else
        die "No sha256sum or shasum found — cannot verify checksum"
    fi

    if [ "${actual}" != "${expected}" ]; then
        die "Checksum mismatch!\n  expected: ${expected}\n  actual:   ${actual}"
    fi
}

# --- Merge env vars into a file without overwriting existing values ---
# Usage: merge_env <file> KEY1=VALUE1 KEY2=VALUE2 ...
merge_env() {
    local env_file="$1"
    shift

    local existing=""
    if [ -f "${env_file}" ]; then
        existing="$(cat "${env_file}")"
    fi

    local appended=false
    local key value
    for pair in "$@"; do
        key="${pair%%=*}"
        value="${pair#*=}"
        # Check if key is already defined (uncommented) in the file
        if [ -n "${existing}" ] && grep -qE "^${key}=" "${env_file}" 2>/dev/null; then
            continue
        fi
        echo "${key}=${value}" >> "${env_file}"
        appended=true
    done

    if [ "${appended}" = true ]; then
        ok "Updated ${env_file}"
    else
        ok "${env_file} already has all required variables"
    fi
}

# --- Install CLI binary ---
install_cli() {
    local tag="$1"
    local target archive checksum_file download_url checksum_url
    local tmpdir expected_sum

    target="$(detect_target)"
    archive="${BIN_NAME}-${target}.tar.gz"

    info "Detected target: ${target}"

    download_url="https://github.com/${REPO}/releases/download/${tag}/${archive}"
    sums_url="https://github.com/${REPO}/releases/download/${tag}/SHA256SUMS"

    tmpdir="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "rm -rf '${tmpdir}'" EXIT

    info "Downloading ${archive}..."
    curl -fSL -o "${tmpdir}/${archive}" "${download_url}"

    info "Downloading checksums..."
    curl -fSL -o "${tmpdir}/SHA256SUMS" "${sums_url}"

    expected_sum="$(grep "${archive}" "${tmpdir}/SHA256SUMS" | cut -d' ' -f1)"
    [ -n "${expected_sum}" ] || die "Checksum for ${archive} not found in SHA256SUMS"
    info "Verifying checksum..."
    verify_checksum "${tmpdir}/${archive}" "${expected_sum}"
    ok "Checksum verified"

    info "Extracting..."
    tar xzf "${tmpdir}/${archive}" -C "${tmpdir}"

    mkdir -p "${INSTALL_DIR}"
    mv "${tmpdir}/${BIN_NAME}" "${INSTALL_DIR}/${BIN_NAME}"
    chmod +x "${INSTALL_DIR}/${BIN_NAME}"
    ok "Installed ${BIN_NAME} to ${INSTALL_DIR}/${BIN_NAME}"

    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            warn "${INSTALL_DIR} is not on your PATH."
            warn "Add it with:  export PATH=\"${INSTALL_DIR}:\${PATH}\""
            ;;
    esac
}

# --- Check docker availability ---
check_docker() {
    if ! command -v docker >/dev/null 2>&1; then
        die "docker is not installed. Install Docker first or use --cli-only to skip server setup."
    fi

    if ! docker compose version >/dev/null 2>&1; then
        die "docker compose plugin is not available. Install it first or use --cli-only to skip server setup."
    fi

    ok "docker and docker compose are available"
}

# --- Set up server via Docker Compose ---
setup_server() {
    local tag="$1"
    local compose_url api_key ollama_key

    info "Setting up server in ${SERVER_DIR}..."
    mkdir -p "${SERVER_DIR}"

    # Download docker-compose.yml
    compose_url="https://raw.githubusercontent.com/${REPO}/${tag}/docker-compose.yml"
    info "Downloading docker-compose.yml..."
    curl -fSL -o "${SERVER_DIR}/docker-compose.yml" "${compose_url}"
    ok "docker-compose.yml saved to ${SERVER_DIR}/docker-compose.yml"

    # Generate API keys if needed
    if [ -f "${SERVER_DIR}/.env" ] && grep -qE '^MEMCAN_API_KEY=' "${SERVER_DIR}/.env" 2>/dev/null; then
        api_key="$(grep -E '^MEMCAN_API_KEY=' "${SERVER_DIR}/.env" | head -1 | cut -d= -f2-)"
    else
        api_key="$(openssl rand -hex 32)"
        info "Generated new MEMCAN_API_KEY"
    fi

    if [ -f "${SERVER_DIR}/.env" ] && grep -qE '^OLLAMA_API_KEY=' "${SERVER_DIR}/.env" 2>/dev/null; then
        ollama_key="$(grep -E '^OLLAMA_API_KEY=' "${SERVER_DIR}/.env" | head -1 | cut -d= -f2-)"
    else
        ollama_key="$(openssl rand -hex 32)"
        info "Generated new OLLAMA_API_KEY"
    fi

    # Merge server .env
    merge_env "${SERVER_DIR}/.env" \
        "MEMCAN_API_KEY=${api_key}" \
        "OLLAMA_API_KEY=${ollama_key}" \
        "OLLAMA_HOST=http://ollama:11434" \
        "LLM_MODEL=ollama::qwen3.5:9b" \
        "EMBED_MODEL=MultilingualE5Large" \
        "DISTILL_MEMORIES=true" \
        "COMPOSE_PROFILES=gpu"

    # Merge CLI .env
    mkdir -p "${CLI_ENV_DIR}"
    merge_env "${CLI_ENV_DIR}/.env" \
        "MEMCAN_API_KEY=${api_key}" \
        "MEMCAN_URL=http://localhost:8190"
}

# --- Main ---
main() {
    local tag

    # Resolve version
    if [ -z "${VERSION}" ]; then
        info "Fetching latest release from GitHub..."
        tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | cut -d'"' -f4)"
        [ -n "${tag}" ] || die "Could not determine latest release tag"
    else
        tag="${VERSION}"
    fi

    info "Version: ${tag}"

    # Check docker before doing anything if server setup is requested
    if [ "${CLI_ONLY}" = false ]; then
        check_docker
    fi

    # Install CLI
    install_cli "${tag}"

    # Server setup
    if [ "${CLI_ONLY}" = false ]; then
        setup_server "${tag}"

        printf '\n%s%smemcan %s installed successfully.%s\n\n' "${GREEN}" "${BOLD}" "${tag}" "${RESET}"
        info "To start the server:"
        printf '\n    cd %s && docker compose up -d\n\n' "${SERVER_DIR}"
    else
        printf '\n%s%smemcan %s CLI installed successfully.%s\n' "${GREEN}" "${BOLD}" "${tag}" "${RESET}"
    fi
}

main
