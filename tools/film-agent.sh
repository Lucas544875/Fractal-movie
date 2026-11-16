#!/usr/bin/env bash
# Shared backend selection. Export it so nested critics use the same backend.
film_select_backend() {
    if [[ -z ${FILM_AGENT_BACKEND:-} ]]; then
        if [[ -n ${CODEX_THREAD_ID:-}${CODEX_SESSION_ID:-} ]]; then
            FILM_AGENT_BACKEND=codex
        else
            FILM_AGENT_BACKEND=claude
        fi
    fi
    case "$FILM_AGENT_BACKEND" in
        codex|claude) ;;
        *) echo "FILM_AGENT_BACKEND must be codex or claude" >&2; return 2 ;;
    esac
    export FILM_AGENT_BACKEND
}

# Match the Claude result envelope used by the caller. Keep events on stderr.
film_codex() (
    set -euo pipefail
    role=$1
    prompt=$2
    model=$3
    shift 3
    result=$(mktemp)
    trap 'rm -f "$result"' EXIT
    args=(exec --ephemeral --json --output-last-message "$result"
        -c approval_policy='"never"')
    if [[ $role == director ]]; then
        args+=(--sandbox workspace-write)
    else
        args+=(--sandbox read-only)
    fi
    if [[ -n $model ]]; then args+=(--model "$model"); fi
    codex "${args[@]}" "$@" "$prompt" >&2
    python3 - "$result" <<'PYRESULT'
import json, pathlib, sys
print(json.dumps({"result": pathlib.Path(sys.argv[1]).read_text()}))
PYRESULT
)
