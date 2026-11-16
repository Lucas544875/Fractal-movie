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

# Consume harness launcher options, forwarding all other arguments unchanged.
film_parse_options() {
    FILM_REMAINING_ARGS=()
    while (( $# )); do
        case "$1" in
            --agent-backend|--agent-config|--model|--director-model|--critic-model)
                if (( $# < 2 )) || [[ -z $2 || $2 == --* ]]; then
                    echo "Missing value for $1" >&2; return 2
                fi
                case "$1" in
                    --agent-backend) export FILM_AGENT_BACKEND=$2 ;;
                    --agent-config) export FILM_AGENT_CONFIG=$2 ;;
                    --model) export FILM_MODEL=$2 ;;
                    --director-model) export FILM_DIRECTOR_MODEL=$2 ;;
                    --critic-model) export FILM_CRITIC_MODEL=$2 ;;
                esac
                shift 2 ;;
            --) shift; FILM_REMAINING_ARGS+=("$@"); break ;;
            *) FILM_REMAINING_ARGS+=("$1"); shift ;;
        esac
    done
}

film_configure() {
    film_select_backend
    local resolved
    resolved=$(python3 - <<'PYCONFIG'
import json, os, pathlib, sys
backend = os.environ['FILM_AGENT_BACKEND']
path = pathlib.Path(os.environ.get('FILM_AGENT_CONFIG', 'film-agents.json'))
config = {}
try:
    if path.exists():
        config = json.loads(path.read_text())
        if not isinstance(config, dict) or set(config) - {'codex', 'claude'}:
            raise ValueError('expected codex and/or claude objects')
        for section in config.values():
            if not isinstance(section, dict) or set(section) - {'model', 'director_model', 'critic_model'}:
                raise ValueError('allowed fields: model, director_model, critic_model')
            if any(not isinstance(v, str) or '\n' in v or '\r' in v for v in section.values()):
                raise ValueError('models must be single-line strings')
    elif 'FILM_AGENT_CONFIG' in os.environ:
        raise ValueError(f'configuration does not exist: {path}')
    section = config.get(backend, {})
    for role in ('director', 'critic'):
        model = (os.environ.get(f'FILM_{role.upper()}_MODEL') or
                 os.environ.get('FILM_MODEL') or
                 os.environ.get(f'FILM_{backend.upper()}_MODEL') or
                 section.get(f'{role}_model') or section.get('model') or '')
        if '\n' in model or '\r' in model:
            raise ValueError('models must be single-line strings')
        print(model)
except (ValueError, OSError) as exc:
    sys.exit(f'Invalid film agent configuration: {exc}')
PYCONFIG
) || return
    FILM_RESOLVED_DIRECTOR_MODEL=${resolved%%$'\n'*}
    FILM_RESOLVED_CRITIC_MODEL=${resolved#*$'\n'}
    # Command substitution strips trailing newlines: preserve an empty critic.
    if [[ $resolved != *$'\n'* ]]; then FILM_RESOLVED_CRITIC_MODEL=; fi
    export FILM_RESOLVED_DIRECTOR_MODEL FILM_RESOLVED_CRITIC_MODEL
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
