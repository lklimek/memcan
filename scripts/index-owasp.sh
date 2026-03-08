#!/usr/bin/env bash
# Index OWASP security standards into MemCan's memcan-standards collection.
#
# Sources (shallow-cloned from GitHub on first run):
#   - OWASP Cheat Sheets: github.com/OWASP/CheatSheetSeries (master)
#   - OWASP ASVS 5.0:     github.com/OWASP/ASVS (v5.0.0 tag)
#
# Usage:
#   ./scripts/index-owasp.sh                    # index everything
#   ./scripts/index-owasp.sh cheatsheets        # index cheat sheets only
#   ./scripts/index-owasp.sh asvs               # index ASVS only
#   ./scripts/index-owasp.sh --drop             # drop all OWASP data
#   ./scripts/index-owasp.sh --drop cheatsheets # drop cheat sheets only
#   ./scripts/index-owasp.sh --drop asvs        # drop ASVS only
#   ./scripts/index-owasp.sh --reindex          # drop then re-index target(s)
#
# Environment:
#   CHEATSHEETS_DIR  override cheat sheets location (skip clone)
#   ASVS_DIR         override ASVS location (skip clone)
#   MEMCAN_DIR      override memcan repo root (default: script's repo)

set -euo pipefail

# --- paths ---

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MEMCAN_DIR="${MEMCAN_DIR:-$(cd "$SCRIPT_DIR/.." && pwd)}"
MCP_SERVER_DIR="$MEMCAN_DIR/mcp-server"
INDEX_SCRIPT="$SCRIPT_DIR/index-standards.py"
CACHE_DIR="$MEMCAN_DIR/.cache"

CHEATSHEETS_DIR="${CHEATSHEETS_DIR:-$CACHE_DIR/owasp-cheatsheets}"
ASVS_DIR="${ASVS_DIR:-$CACHE_DIR/owasp-asvs-5.0}"

# --- constants ---

CHEATSHEETS_REPO="https://github.com/OWASP/CheatSheetSeries.git"
CHEATSHEETS_STANDARD_ID="owasp-cheatsheets"
CHEATSHEETS_VERSION="2024"

ASVS_REPO="https://github.com/OWASP/ASVS.git"
ASVS_TAG="v5.0.0"
ASVS_STANDARD_ID="owasp-asvs"
ASVS_VERSION="5.0"

# --- helpers ---

log()  { echo "[$(date +%H:%M:%S)] $*"; }
fail() { log "ERROR: $*" >&2; exit 1; }

run_indexer() {
    local file="$1"; shift
    cd "$MCP_SERVER_DIR"
    uv run python "$INDEX_SCRIPT" "$file" "$@"
}

drop_standard() {
    local standard_id="$1"
    log "Dropping all points for standard-id=$standard_id ..."
    cd "$MCP_SERVER_DIR"
    uv run python "$INDEX_SCRIPT" --drop --standard-id "$standard_id"
    log "Drop complete: $standard_id"
}

# --- clone helpers ---

clone_cheatsheets() {
    if [ -d "$CHEATSHEETS_DIR/cheatsheets" ]; then
        log "Cheat Sheets already cloned to $CHEATSHEETS_DIR"
        return 0
    fi

    log "Cloning OWASP CheatSheetSeries (shallow) ..."
    mkdir -p "$CACHE_DIR"
    git clone --depth 1 "$CHEATSHEETS_REPO" "$CHEATSHEETS_DIR"
    log "Cloned to $CHEATSHEETS_DIR"
}

clone_asvs() {
    if [ -d "$ASVS_DIR/5.0" ]; then
        log "ASVS already cloned to $ASVS_DIR"
        return 0
    fi

    log "Cloning OWASP ASVS $ASVS_TAG (shallow) ..."
    mkdir -p "$CACHE_DIR"
    git clone --depth 1 --branch "$ASVS_TAG" "$ASVS_REPO" "$ASVS_DIR"
    log "Cloned to $ASVS_DIR"
}

# --- index functions ---

index_cheatsheets() {
    clone_cheatsheets

    local src_dir="$CHEATSHEETS_DIR/cheatsheets"
    [ -d "$src_dir" ] || fail "Cheatsheets dir not found: $src_dir"

    local count=0 failed=0 total
    total=$(find "$src_dir" -maxdepth 1 -name '*.md' ! -iname 'Index*' | wc -l)

    log "Indexing $total OWASP Cheat Sheets"
    log "  Source: $src_dir"
    log "  Standard: $CHEATSHEETS_STANDARD_ID | Version: $CHEATSHEETS_VERSION"
    echo

    for file in "$src_dir"/*.md; do
        # Skip index/readme files
        [[ "$(basename "$file")" =~ ^(Index|README) ]] && continue
        ((count++))

        log "[$count/$total] $(basename "$file")"
        if run_indexer "$file" \
            --standard-id "$CHEATSHEETS_STANDARD_ID" \
            --standard-type security \
            --version "$CHEATSHEETS_VERSION" \
            --lang en \
            --url "https://cheatsheetseries.owasp.org/"; then
            :
        else
            ((failed++))
            log "FAILED: $(basename "$file")"
        fi
    done

    echo
    log "Cheat Sheets done. Indexed: $((count - failed)) | Failed: $failed"
    return $failed
}

index_asvs() {
    clone_asvs

    local src_dir="$ASVS_DIR/5.0/en"
    [ -d "$src_dir" ] || fail "ASVS dir not found: $src_dir"

    local count=0 failed=0 total
    total=$(find "$src_dir" -maxdepth 1 -name '0x1*.md' | wc -l)

    log "Indexing $total OWASP ASVS 5.0 chapters"
    log "  Source: $src_dir"
    log "  Standard: $ASVS_STANDARD_ID | Version: $ASVS_VERSION"
    echo

    for file in "$src_dir"/0x1*.md; do
        ((count++))

        log "[$count/$total] $(basename "$file")"
        if run_indexer "$file" \
            --standard-id "$ASVS_STANDARD_ID" \
            --standard-type security \
            --version "$ASVS_VERSION" \
            --lang en \
            --url "https://github.com/OWASP/ASVS/tree/v5.0.0"; then
            :
        else
            ((failed++))
            log "FAILED: $(basename "$file")"
        fi
    done

    echo
    log "ASVS done. Indexed: $((count - failed)) | Failed: $failed"
    return $failed
}

# --- main ---

DROP=false
REINDEX=false
TARGET=""

for arg in "$@"; do
    case "$arg" in
        --drop)       DROP=true ;;
        --reindex)    REINDEX=true ;;
        --help|-h)    sed -n '2,20s/^# //p' "$0"; exit 0 ;;
        cheatsheets)  TARGET="cheatsheets" ;;
        asvs)         TARGET="asvs" ;;
        *)            fail "Unknown argument: $arg" ;;
    esac
done

# Handle --reindex (drop + index)
if $REINDEX; then
    case "$TARGET" in
        cheatsheets) drop_standard "$CHEATSHEETS_STANDARD_ID" ;;
        asvs)        drop_standard "$ASVS_STANDARD_ID" ;;
        "")          drop_standard "$CHEATSHEETS_STANDARD_ID"
                     drop_standard "$ASVS_STANDARD_ID" ;;
    esac
fi

# Handle --drop (drop only, no indexing)
if $DROP; then
    case "$TARGET" in
        cheatsheets) drop_standard "$CHEATSHEETS_STANDARD_ID" ;;
        asvs)        drop_standard "$ASVS_STANDARD_ID" ;;
        "")          drop_standard "$CHEATSHEETS_STANDARD_ID"
                     drop_standard "$ASVS_STANDARD_ID" ;;
    esac
    exit 0
fi

failed=0
case "$TARGET" in
    cheatsheets) index_cheatsheets || ((failed++)) ;;
    asvs)        index_asvs || ((failed++)) ;;
    "")
        index_cheatsheets || ((failed++))
        echo
        echo "==============================="
        echo
        index_asvs || ((failed++))
        ;;
esac

echo
echo "==============================="
log "All done."
[ "$failed" -gt 0 ] && exit 1
exit 0
