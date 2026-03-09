#!/usr/bin/env bash
# Index WCAG and CVSS standards into MemCan's memcan-standards collection.
#
# Usage:
#   ./scripts/index-standards.sh                    # index everything
#   ./scripts/index-standards.sh wcag               # WCAG only
#   ./scripts/index-standards.sh cvss               # CVSS only
#   ./scripts/index-standards.sh --drop             # drop all
#   ./scripts/index-standards.sh --drop wcag        # drop WCAG only
#   ./scripts/index-standards.sh --drop cvss        # drop CVSS only
#   ./scripts/index-standards.sh --reindex          # drop then re-index
#   ./scripts/index-standards.sh --reindex wcag     # drop then re-index WCAG only
#
# Environment:
#   MEMCAN_DIR       override memcan repo root (default: script's parent dir)
#   CARGO_TARGET_DIR if set, use $CARGO_TARGET_DIR/debug/memcan for CLI
#   CACHE_DIR        cache directory (default: $MEMCAN_DIR/.cache)
#   BATCH_SIZE       parallel jobs per batch (default: 16)
#   BATCH_TIMEOUT    seconds to wait per batch (default: 1800 = 30min)

set -euo pipefail

# --- paths ---

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MEMCAN_DIR="${MEMCAN_DIR:-$(cd "$SCRIPT_DIR/.." && pwd)}"
CACHE_DIR="${CACHE_DIR:-$MEMCAN_DIR/.cache}"

if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    MEMCAN_CLI="$CARGO_TARGET_DIR/debug/memcan"
else
    MEMCAN_CLI="$MEMCAN_DIR/target/debug/memcan"
fi

BATCH_SIZE="${BATCH_SIZE:-16}"
BATCH_TIMEOUT="${BATCH_TIMEOUT:-1800}"

# --- constants ---

WCAG_REPO="https://github.com/w3c/wcag.git"
WCAG_CACHE_DIR="$CACHE_DIR/w3c-wcag"
WCAG_MD_DIR="$CACHE_DIR/wcag-md"
WCAG_STANDARD_ID="wcag"
WCAG_VERSION="2.2"

CVSS_URL="https://www.first.org/cvss/v4.0/specification-document"
CVSS_CACHE_HTML="$CACHE_DIR/cvss-v4-spec.html"
CVSS_CACHE_MD="$CACHE_DIR/cvss-v4-spec.md"
CVSS_STANDARD_ID="cvss"
CVSS_VERSION="4.0"

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

check_pandoc() {
    command -v pandoc >/dev/null 2>&1 || fail "pandoc not found. Install it:
  Ubuntu/Debian: sudo apt install pandoc
  macOS:         brew install pandoc
  Other:         https://pandoc.org/installing.html"
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

    declare -A pending=()
    for op in "${ops[@]}"; do
        [ -n "$op" ] && pending[$op]=1
    done

    while [ "${#pending[@]}" -gt 0 ]; do
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
            step=$(echo "$status_json" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('step',d.get('status','')))" 2>/dev/null) || true

            # Treat empty/error response as "expired from LRU" → assume completed
            if [ -z "$step" ] || echo "$status_json" | grep -q '"error"'; then
                ((completed++)) || true
                unset "pending[$op]"
                log "  EXPIRED: $op (evicted from server LRU, assuming completed)"
                continue
            fi

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

# --- clone / fetch helpers ---

clone_wcag() {
    if [ -d "$WCAG_CACHE_DIR/understanding" ]; then
        log "WCAG already cloned to $WCAG_CACHE_DIR"
        return 0
    fi
    log "Cloning W3C WCAG (shallow, branch: main) ..."
    mkdir -p "$CACHE_DIR"
    git clone --depth 1 --branch main "$WCAG_REPO" "$WCAG_CACHE_DIR"
    log "Cloned to $WCAG_CACHE_DIR"
}

convert_wcag_html() {
    mkdir -p "$WCAG_MD_DIR"
    local converted=0
    local skipped=0

    for dir in "$WCAG_CACHE_DIR"/understanding/20 "$WCAG_CACHE_DIR"/understanding/21 "$WCAG_CACHE_DIR"/understanding/22; do
        [ -d "$dir" ] || continue
        for html_file in "$dir"/*.html; do
            [ -f "$html_file" ] || continue
            local basename
            basename=$(basename "$html_file")

            # skip index.html
            [ "$basename" = "index.html" ] && continue

            # skip files < 500 bytes (empty stubs)
            local filesize
            filesize=$(wc -c < "$html_file")
            if [ "$filesize" -lt 500 ]; then
                ((skipped++)) || true
                continue
            fi

            local md_file="$WCAG_MD_DIR/${basename%.html}.md"

            # reuse converted file if source hasn't changed
            if [ -f "$md_file" ] && [ "$md_file" -nt "$html_file" ]; then
                ((skipped++)) || true
                continue
            fi

            pandoc -f html -t markdown --wrap=none "$html_file" -o "$md_file"
            ((converted++)) || true
        done
    done

    log "Converted $converted HTML files to markdown ($skipped skipped)"
}

fetch_cvss() {
    if [ -f "$CVSS_CACHE_HTML" ]; then
        log "CVSS spec already cached at $CVSS_CACHE_HTML"
    else
        log "Fetching CVSS v4.0 specification ..."
        mkdir -p "$CACHE_DIR"
        curl -sL "$CVSS_URL" -o "$CVSS_CACHE_HTML"
        log "Saved to $CVSS_CACHE_HTML"
    fi

    # convert to markdown (always regenerate if HTML is newer)
    if [ ! -f "$CVSS_CACHE_MD" ] || [ "$CVSS_CACHE_HTML" -nt "$CVSS_CACHE_MD" ]; then
        log "Converting CVSS spec to markdown ..."
        pandoc --from html --to markdown --wrap=none "$CVSS_CACHE_HTML" -o "$CVSS_CACHE_MD"
        log "Saved to $CVSS_CACHE_MD"
    fi
}

# --- index functions ---

TOTAL_FAILED=0

index_wcag() {
    clone_wcag
    convert_wcag_html

    local -a files=()
    for file in "$WCAG_MD_DIR"/*.md; do
        [ -f "$file" ] || continue
        files+=("$file")
    done

    [ ${#files[@]} -eq 0 ] && fail "No WCAG markdown files found in $WCAG_MD_DIR"

    log "Indexing ${#files[@]} WCAG 2.2 understanding documents"
    log "  Source: $WCAG_MD_DIR"
    log "  Standard: $WCAG_STANDARD_ID | Version: $WCAG_VERSION"
    echo

    INDEXER_ARGS=(
        --standard-id "$WCAG_STANDARD_ID"
        --standard-type accessibility
        --version "$WCAG_VERSION"
        --lang en
        --url "https://www.w3.org/WAI/WCAG22/Understanding/"
    )

    process_batch "${files[@]}"
    log "WCAG done. Failed: $TOTAL_FAILED"
}

index_cvss() {
    fetch_cvss

    [ -f "$CVSS_CACHE_MD" ] || fail "CVSS markdown not found at $CVSS_CACHE_MD"

    log "Indexing CVSS v4.0 specification"
    log "  Source: $CVSS_CACHE_MD"
    log "  Standard: $CVSS_STANDARD_ID | Version: $CVSS_VERSION"
    echo

    log "  Submitting: $(basename "$CVSS_CACHE_MD")"
    "$MEMCAN_CLI" index-standards "$CVSS_CACHE_MD" \
        --standard-id "$CVSS_STANDARD_ID" \
        --standard-type security \
        --version "$CVSS_VERSION" \
        --lang en \
        --url "$CVSS_URL" \
        --wait

    log "CVSS done."
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
        wcag)         TARGET="wcag" ;;
        cvss)         TARGET="cvss" ;;
        *)            fail "Unknown argument: $arg" ;;
    esac
done

check_cli
check_pandoc
check_server

if $REINDEX; then
    case "$TARGET" in
        wcag) drop_standard "$WCAG_STANDARD_ID" ;;
        cvss) drop_standard "$CVSS_STANDARD_ID" ;;
        "")   drop_standard "$WCAG_STANDARD_ID"
              drop_standard "$CVSS_STANDARD_ID" ;;
    esac
fi

if $DROP; then
    case "$TARGET" in
        wcag) drop_standard "$WCAG_STANDARD_ID" ;;
        cvss) drop_standard "$CVSS_STANDARD_ID" ;;
        "")   drop_standard "$WCAG_STANDARD_ID"
              drop_standard "$CVSS_STANDARD_ID" ;;
    esac
    exit 0
fi

TOTAL_FAILED=0
case "$TARGET" in
    wcag) index_wcag ;;
    cvss) index_cvss ;;
    "")
        index_wcag
        echo
        echo "==============================="
        echo
        index_cvss
        ;;
esac

echo
echo "==============================="
log "All done. Total failures: $TOTAL_FAILED"
[ "$TOTAL_FAILED" -gt 0 ] && exit 1
exit 0
