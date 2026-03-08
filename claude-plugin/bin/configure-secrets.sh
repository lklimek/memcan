#!/usr/bin/env bash
# MindOJO secret configuration — resolves API keys, writes .env, updates settings.json.
# Never prints secret values to stdout/stderr.
set -euo pipefail

ENV_DIR="$HOME/.config/mindojo"
ENV_FILE="$ENV_DIR/.env"
SETTINGS_FILE="$HOME/.claude/settings.json"
PREFIX="[configure-secrets]"

# --- Helpers ---

generate_random_hex() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 32
    elif command -v xxd >/dev/null 2>&1; then
        head -c 32 /dev/urandom | xxd -p -c 64
    else
        echo "$PREFIX ERROR: Neither openssl nor xxd available for key generation." >&2
        exit 1
    fi
}

read_env_value() {
    local key="$1"
    local file="$2"
    if [ -f "$file" ]; then
        # Match KEY=value (possibly quoted), skip commented lines
        grep -E "^${key}=" "$file" 2>/dev/null | tail -1 | sed "s/^${key}=//" | sed 's/^"//;s/"$//' | sed "s/^'//;s/'$//" || true
    fi
}

update_env_line() {
    local key="$1"
    local value="$2"
    local file="$3"
    if grep -qE "^${key}=" "$file" 2>/dev/null; then
        # Replace existing line (use | as sed delimiter to avoid issues with / in URLs)
        sed -i "s|^${key}=.*|${key}=${value}|" "$file"
    elif grep -qE "^#\s*${key}=" "$file" 2>/dev/null; then
        # Uncomment and set
        sed -i "s|^#\s*${key}=.*|${key}=${value}|" "$file"
    else
        echo "${key}=${value}" >> "$file"
    fi
}

# --- Step 1: Resolve MINDOJO_API_KEY ---

resolve_mindojo_api_key() {
    local val=""
    local source=""

    val="$(read_env_value MINDOJO_API_KEY "$ENV_FILE")"
    if [ -n "$val" ]; then
        source="existing"
    fi

    if [ -z "$val" ] && [ -n "${MINDOJO_API_KEY:-}" ]; then
        val="$MINDOJO_API_KEY"
        source="existing"
    fi

    if [ -z "$val" ]; then
        val="$(generate_random_hex)"
        source="generated"
    fi

    RESOLVED_MINDOJO_API_KEY="$val"
    RESOLVED_MINDOJO_API_KEY_SOURCE="$source"
}

# --- Step 2: Resolve OLLAMA_API_KEY ---

resolve_ollama_api_key() {
    local val=""

    val="$(read_env_value OLLAMA_API_KEY "$ENV_FILE")"
    if [ -z "$val" ] && [ -n "${OLLAMA_API_KEY:-}" ]; then
        val="$OLLAMA_API_KEY"
    fi

    RESOLVED_OLLAMA_API_KEY="$val"
}

# --- Step 3: Resolve MINDOJO_URL ---

resolve_mindojo_url() {
    local val=""

    val="$(read_env_value MINDOJO_URL "$ENV_FILE")"
    if [ -z "$val" ] && [ -n "${MINDOJO_URL:-}" ]; then
        val="$MINDOJO_URL"
    fi

    RESOLVED_MINDOJO_URL="${val:-http://localhost:8190}"
}

# --- Step 4: Write/update .env ---

write_env_file() {
    mkdir -p "$ENV_DIR"

    if [ -f "$ENV_FILE" ]; then
        # Update existing file, preserving all other lines
        update_env_line "MINDOJO_API_KEY" "$RESOLVED_MINDOJO_API_KEY" "$ENV_FILE"
        update_env_line "MINDOJO_URL" "$RESOLVED_MINDOJO_URL" "$ENV_FILE"
        if [ -n "$RESOLVED_OLLAMA_API_KEY" ]; then
            update_env_line "OLLAMA_API_KEY" "$RESOLVED_OLLAMA_API_KEY" "$ENV_FILE"
        fi
    else
        # Create new file with resolved values and commented templates
        {
            echo "# MindOJO configuration"
            echo "# See: https://github.com/lklimek/mindojo"
            echo ""
            echo "# Server connection"
            echo "MINDOJO_API_KEY=$RESOLVED_MINDOJO_API_KEY"
            echo "MINDOJO_URL=$RESOLVED_MINDOJO_URL"
            echo ""
            echo "# Ollama"
            echo "# OLLAMA_HOST=http://localhost:11434"
            if [ -n "$RESOLVED_OLLAMA_API_KEY" ]; then
                echo "OLLAMA_API_KEY=$RESOLVED_OLLAMA_API_KEY"
            else
                echo "# OLLAMA_API_KEY="
            fi
            echo "# LLM_MODEL=ollama::qwen3.5:4b"
            echo ""
            echo "# Embeddings"
            echo "# EMBED_MODEL=MultilingualE5Large"
            echo ""
            echo "# Logging"
            echo "# MINDOJO_LOG_FILE="
        } > "$ENV_FILE"
    fi

    echo "$PREFIX .env: MINDOJO_API_KEY=<$RESOLVED_MINDOJO_API_KEY_SOURCE> MINDOJO_URL=$RESOLVED_MINDOJO_URL"
}

# --- Step 5: Merge into settings.json ---

update_settings_json() {
    local json_tool=""

    if command -v jq >/dev/null 2>&1; then
        json_tool="jq"
    elif command -v python3 >/dev/null 2>&1; then
        json_tool="python3"
    else
        echo "$PREFIX ERROR: Neither jq nor python3 available for JSON manipulation." >&2
        exit 1
    fi

    mkdir -p "$(dirname "$SETTINGS_FILE")"

    local existing="{}"
    if [ -f "$SETTINGS_FILE" ]; then
        existing="$(cat "$SETTINGS_FILE")"
        # Validate JSON
        if [ "$json_tool" = "jq" ]; then
            if ! echo "$existing" | jq empty 2>/dev/null; then
                echo "$PREFIX ERROR: $SETTINGS_FILE contains invalid JSON." >&2
                exit 1
            fi
        else
            if ! python3 -c "import json, sys; json.loads(sys.stdin.read())" <<< "$existing" 2>/dev/null; then
                echo "$PREFIX ERROR: $SETTINGS_FILE contains invalid JSON." >&2
                exit 1
            fi
        fi
    fi

    local updated=""
    if [ "$json_tool" = "jq" ]; then
        updated="$(echo "$existing" | jq \
            --arg key "$RESOLVED_MINDOJO_API_KEY" \
            --arg url "$RESOLVED_MINDOJO_URL" \
            '.env = ((.env // {}) + {MINDOJO_API_KEY: $key, MINDOJO_URL: $url})')"
    else
        updated="$(python3 -c "
import json, sys
data = json.loads(sys.stdin.read())
env = data.get('env', {})
env['MINDOJO_API_KEY'] = sys.argv[1]
env['MINDOJO_URL'] = sys.argv[2]
data['env'] = env
print(json.dumps(data, indent=2))
" "$RESOLVED_MINDOJO_API_KEY" "$RESOLVED_MINDOJO_URL" <<< "$existing")"
    fi

    echo "$updated" > "$SETTINGS_FILE"
    echo "$PREFIX settings.json: updated env block"
}

# --- Main ---

main() {
    resolve_mindojo_api_key
    resolve_ollama_api_key
    resolve_mindojo_url
    write_env_file
    update_settings_json
    echo "$PREFIX Done."
}

main
