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
#   BATCH_SIZE       parallel jobs per batch (default: 16)
#   BATCH_TIMEOUT    seconds to wait per batch (default: 1800 = 30min)

set -euo pipefail

# --- paths ---

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MEMCAN_DIR="${MEMCAN_DIR:-$(cd "$SCRIPT_DIR/.." && pwd)}"
CACHE_DIR="$MEMCAN_DIR/.cache"

if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    MEMCAN_CLI="$CARGO_TARGET_DIR/debug/memcan"
else
    MEMCAN_CLI="$MEMCAN_DIR/target/debug/memcan"
fi

CHEATSHEETS_DIR="${CHEATSHEETS_DIR:-$CACHE_DIR/owasp-cheatsheets}"
ASVS_DIR="${ASVS_DIR:-$CACHE_DIR/owasp-asvs-5.0}"

BATCH_SIZE="${BATCH_SIZE:-16}"
BATCH_TIMEOUT="${BATCH_TIMEOUT:-1800}"

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

check_server() {
    local url="${MEMCAN_URL:-http://localhost:8190}"
    if ! curl -so /dev/null -w '' "${url}/health" 2>/dev/null; then
        fail "memcan-server not reachable at $url — start it first"
    fi
    log "Server reachable at $url"
}

check_cli() {
    [ -x "$MEMCAN_CLI" ] || fail "memcan CLI not found at $MEMCAN_CLI — run: cargo build -p memcan"
}

submit_file() {
    local file="$1"; shift
    local result
    result=$("$MEMCAN_CLI" index-standards "$file" "$@" 2>/dev/null) || true
    echo "$result" | python3 -c "import sys,json; print(json.load(sys.stdin).get('operation_id',''))" 2>/dev/null || echo ""
}

wait_for_ops() {
    local timeout="$1"; shift
    local ops=("$@")
    local start_time
    start_time=$(date +%s)
    local completed=0
    local failed=0
    local total=${#ops[@]}

    declare -A pending
    for op in "${ops[@]}"; do
        [ -n "$op" ] && pending[$op]=1
    done

    while [ ${#pending[@]} -gt 0 ]; do
        local now
        now=$(date +%s)
        local elapsed=$(( now - start_time ))
        if [ "$elapsed" -ge "$timeout" ]; then
            log "TIMEOUT: $elapsed seconds elapsed, ${#pending[@]} operations still pending"
            return $(( failed + ${#pending[@]} ))
        fi

        sleep 3

        for op in "${!pending[@]}"; do
            local status_json
            status_json=$("$MEMCAN_CLI" status "$op" 2>/dev/null) || continue
            local step
            step=$(echo "$status_json" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('step',d.get('status','')))" 2>/dev/null) || continue

            case "$step" in
                completed|completed_degraded)
                    ((completed++)) || true
                    unset "pending[$op]"
                    ;;
                failed)
                    ((failed++)) || true
                    unset "pending[$op]"
                    log "  FAILED: $op"
                    ;;
            esac
        done

        log "  Progress: $completed/$total completed, $failed failed, ${#pending[@]} pending (${elapsed}s)"
    done

    return "$failed"
}

process_batch() {
    local -a files=("$@")
    local total=${#files[@]}
    local batch_num=0

    log "Processing $total files in batches of $BATCH_SIZE"

    local i=0
    while [ "$i" -lt "$total" ]; do
        ((batch_num++)) || true
        local batch_end=$(( i + BATCH_SIZE ))
        [ "$batch_end" -gt "$total" ] && batch_end="$total"
        local batch_size=$(( batch_end - i ))

        log "=== Batch $batch_num: files $((i+1))-$batch_end of $total ==="

        local -a op_ids=()
        local j="$i"
        while [ "$j" -lt "$batch_end" ]; do
            local file="${files[$j]}"
            log "  Submitting: $(basename "$file")"
            local op_id
            op_id=$(submit_file "$file" "${INDEXER_ARGS[@]}")
            op_ids+=("$op_id")
            ((j++)) || true
        done

        log "Waiting for batch $batch_num ($batch_size files, timeout ${BATCH_TIMEOUT}s)..."
        local batch_failed=0
        wait_for_ops "$BATCH_TIMEOUT" "${op_ids[@]}" || batch_failed=$?
        if [ "$batch_failed" -gt 0 ]; then
            log "Batch $batch_num: $batch_failed failures"
            TOTAL_FAILED=$((TOTAL_FAILED + batch_failed))
        fi
        log "Batch $batch_num complete"
        echo

        i="$batch_end"
    done
}

drop_standard() {
    local standard_id="$1"
    log "Dropping all points for standard-id=$standard_id ..."
    "$MEMCAN_CLI" index-standards --drop --standard-id "$standard_id"
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

TOTAL_FAILED=0

index_cheatsheets() {
    clone_cheatsheets
    local src_dir="$CHEATSHEETS_DIR/cheatsheets"
    [ -d "$src_dir" ] || fail "Cheatsheets dir not found: $src_dir"

    local -a files=()
    for file in "$src_dir"/*.md; do
        [[ "$(basename "$file")" =~ ^(Index|README) ]] && continue
        files+=("$file")
    done

    log "Indexing ${#files[@]} OWASP Cheat Sheets"
    log "  Source: $src_dir"
    log "  Standard: $CHEATSHEETS_STANDARD_ID | Version: $CHEATSHEETS_VERSION"
    echo

    INDEXER_ARGS=(
        --standard-id "$CHEATSHEETS_STANDARD_ID"
        --standard-type security
        --version "$CHEATSHEETS_VERSION"
        --lang en
        --url "https://cheatsheetseries.owasp.org/"
    )

    process_batch "${files[@]}"
    log "Cheat Sheets done. Failed: $TOTAL_FAILED"
}

index_asvs() {
    clone_asvs
    local src_dir="$ASVS_DIR/5.0/en"
    [ -d "$src_dir" ] || fail "ASVS dir not found: $src_dir"

    local -a files=()
    for file in "$src_dir"/0x1*.md; do
        files+=("$file")
    done

    log "Indexing ${#files[@]} OWASP ASVS 5.0 chapters"
    log "  Source: $src_dir"
    log "  Standard: $ASVS_STANDARD_ID | Version: $ASVS_VERSION"
    echo

    INDEXER_ARGS=(
        --standard-id "$ASVS_STANDARD_ID"
        --standard-type security
        --version "$ASVS_VERSION"
        --lang en
        --url "https://github.com/OWASP/ASVS/tree/v5.0.0"
    )

    process_batch "${files[@]}"
    log "ASVS done. Failed: $TOTAL_FAILED"
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

check_cli
check_server

if $REINDEX; then
    case "$TARGET" in
        cheatsheets) drop_standard "$CHEATSHEETS_STANDARD_ID" ;;
        asvs)        drop_standard "$ASVS_STANDARD_ID" ;;
        "")          drop_standard "$CHEATSHEETS_STANDARD_ID"
                     drop_standard "$ASVS_STANDARD_ID" ;;
    esac
fi

if $DROP; then
    case "$TARGET" in
        cheatsheets) drop_standard "$CHEATSHEETS_STANDARD_ID" ;;
        asvs)        drop_standard "$ASVS_STANDARD_ID" ;;
        "")          drop_standard "$CHEATSHEETS_STANDARD_ID"
                     drop_standard "$ASVS_STANDARD_ID" ;;
    esac
    exit 0
fi

TOTAL_FAILED=0
case "$TARGET" in
    cheatsheets) index_cheatsheets ;;
    asvs)        index_asvs ;;
    "")
        index_cheatsheets
        echo
        echo "==============================="
        echo
        index_asvs
        ;;
esac

echo
echo "==============================="
log "All done. Total failures: $TOTAL_FAILED"
[ "$TOTAL_FAILED" -gt 0 ] && exit 1
exit 0
