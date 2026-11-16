#!/usr/bin/env bash
# Produces one film autonomously: creates the production and hands it to a
# headless director that may only run the harness, look at its own
# production, and write its three design files.
#
# usage: tools/make-film.sh <production-id> [init options...]
#   e.g. tools/make-film.sh amber-shelf --seed 7
#
# FILM_DIRECTOR_MODEL and FILM_CRITIC_MODEL choose the models of the two
# roles (for example "sonnet"); unset, each uses the session default.
#
# Run from the repository root. Set WGPU_BACKEND (for example to "gl" under
# WSL) before calling if the default backend does not find the GPU.
set -euo pipefail
source tools/film-agent.sh
film_parse_options "$@"
set -- "${FILM_REMAINING_ARGS[@]}"
film_configure

if [[ $# -lt 1 ]]; then
    echo "usage: tools/make-film.sh <production-id> [init options...]" >&2
    exit 2
fi
id=$1
shift
harness=./target/release/fractal-harness
root=productions

cargo build --release -p fractal-renderer-harness
if [[ ! -d $root/$id ]]; then
    "$harness" init "$id" "$@" >/dev/null
fi

# The director is told what it may touch by permission rules, not by
# request: everything that could turn a new film into a variation of an
# old one is unreadable.
settings=$(python3 - "$root" "$id" <<'PY'
import json, os, sys
root, production = sys.argv[1:]
own = f"./{root}/{production}"
others = sorted(
    name for name in os.listdir(root)
    if name != production and os.path.isdir(os.path.join(root, name))
)
print(json.dumps({"permissions": {
    "allow": [
        "Bash(./target/release/fractal-harness *)",
        "Bash(./tools/film-critic.sh *)",
        f"Read({own}/**)",
        f"Edit({own}/brief.yaml)",
        f"Edit({own}/shotlist.yaml)",
        f"Edit({own}/look.yaml)",
    ],
    "deny": [
        # Design decisions that are not the director's to make or grade.
        "Bash(./target/release/fractal-harness init *)",
        "Bash(./target/release/fractal-harness ledger *)",
        "Bash(./target/release/fractal-harness measure *)",
        "Bash(./target/release/fractal-harness tuning write *)",
        # Requests are filed with `request`; deciding about them is not the
        # director's business.
        "Bash(./target/release/fractal-harness vocabulary *)",
        # Production runs without the director; see the end of this script.
        "Bash(./target/release/fractal-harness produce *)",
        "Bash(./target/release/fractal-harness encode *)",
        # Formulas, cameras, and quality the harness derived.
        f"Read({own}/compiled/**)",
        # Past work and other productions.
        *[f"Read(./{root}/{name}/**)" for name in others],
        "Read(./scenes/**)",
        "Read(./output/**)",
        "Read(./portfolio/**)",
        # How the harness judges, which invites aiming at thresholds.
        "Read(./renderer-core/**)",
        "Read(./renderer-cli/**)",
        "Read(./renderer-harness/**)",
        "Read(./docs/**)",
        "Read(./tools/**)",
        "Read(./README.md)",
        "Read(./harness-tuning.yaml)",
    ],
}}))
PY
)

# Harness commands such as the proof run for minutes; production rendering
# is chunked by the director so that no single call exceeds this.
export BASH_DEFAULT_TIMEOUT_MS=600000
export BASH_MAX_TIMEOUT_MS=600000

# A production may take several sessions; each keeps its own record.
session=$(($(find "$root/$id" -maxdepth 1 -name 'director-run-*.json' | wc -l) + 1))

# The director designs the film. A session can end early, for example at a
# usage limit; everything it decided is on disk, so running this script
# again continues where it stopped.
if [[ $FILM_AGENT_BACKEND == codex ]]; then
    prompt=$(python3 - "$id" <<'PYPROMPT'
from pathlib import Path
import sys
text = Path('.claude/skills/fractal-film/SKILL.md').read_text()
text = text.split('---', 2)[-1].replace('$ARGUMENTS', sys.argv[1])
print(text)
print("Use Codex image viewing for Read of images and apply_patch for Write/Edit. "
      "Read only this production's artifacts, excluding compiled/. "
      "Do not read source code, tools, docs, tuning, portfolio, scenes, or other productions. "
      "Only edit brief.yaml, shotlist.yaml and look.yaml. "
      "Run only the harness's permitted stage commands and tools/film-critic.sh. "
      "Do not run init, ledger, measure, tuning write, vocabulary, produce, encode, or review submit.")
PYPROMPT
)
    film_codex director "$prompt" "${FILM_RESOLVED_DIRECTOR_MODEL}" \
        >"$root/$id/director-run-$session.json"
else
model_args=()
if [[ -n $FILM_RESOLVED_DIRECTOR_MODEL ]]; then
    model_args=(--model "$FILM_RESOLVED_DIRECTOR_MODEL")
fi
claude -p "/fractal-film $id" \
    --settings "$settings" \
    --tools "Bash,Read,Write,Edit" \
    --permission-mode dontAsk \
    --max-turns 600 \
    "${model_args[@]}" \
    --output-format json >"$root/$id/director-run-$session.json"
fi

# Rendering and encoding involve no decisions, so no model is involved.
stage() {
    "$harness" next "$id" | python3 -c 'import json, sys; print(json.load(sys.stdin)["stage"])'
}
if [[ $(stage) == production ]]; then
    "$harness" produce "$id" >"$root/$id/produce.json"
fi
if [[ $(stage) == encode ]]; then
    "$harness" encode "$id" >"$root/$id/encode.json"
fi
"$harness" next "$id"
