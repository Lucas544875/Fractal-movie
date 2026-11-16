#!/usr/bin/env bash
# Works through open vocabulary requests with a headless Claude developer.
# The developer implements accepted requests on a branch of its own; merging
# is left to a person.
#
# usage: tools/grow-vocabulary.sh [request numbers...]
#
# Run from the repository root on a clean working tree. Set WGPU_BACKEND (for
# example to "gl" under WSL) before calling if the default backend does not
# find the GPU. VOCAB_DEVELOPER_MODEL chooses the model; unset, the session
# default is used.
set -euo pipefail

if [[ -n $(git status --porcelain) ]]; then
    echo "the working tree has uncommitted changes; commit or stash them first" >&2
    exit 1
fi
started_on=$(git rev-parse --abbrev-ref HEAD)

# The developer may build, test, and commit, but not publish or rewrite
# history, and may not touch productions or the ledger of finished films.
settings='{"permissions": {
  "allow": [
    "Bash(cargo *)",
    "Bash(./target/release/fractal-harness *)",
    "Bash(git status*)", "Bash(git diff*)", "Bash(git log*)",
    "Bash(git switch -c vocab/*)", "Bash(git add *)", "Bash(git commit *)",
    "Read", "Edit(./renderer-core/**)", "Edit(./renderer-harness/**)",
    "Edit(./docs/**)", "Edit(./.claude/skills/**)", "Edit(./harness-tuning.yaml)"
  ],
  "deny": [
    "Bash(git push*)", "Bash(git reset*)", "Bash(git checkout*)", "Bash(git rebase*)",
    "Bash(./target/release/fractal-harness ledger import*)",
    "Edit(./productions/**)", "Edit(./portfolio/ledger.jsonl)"
  ]
}}'

export BASH_DEFAULT_TIMEOUT_MS=600000
export BASH_MAX_TIMEOUT_MS=600000
mkdir -p output/vocab
log=output/vocab/developer-run-$(date +%Y%m%d-%H%M%S).json

claude -p "/fractal-vocab $*" \
    --settings "$settings" \
    --tools "Bash,Read,Write,Edit,Glob,Grep" \
    --permission-mode dontAsk \
    --max-turns 400 \
    ${VOCAB_DEVELOPER_MODEL:+--model "$VOCAB_DEVELOPER_MODEL"} \
    --output-format json >"$log" || true

echo "developer session recorded in $log"
echo "started on branch: $started_on; now on: $(git rev-parse --abbrev-ref HEAD)"
./target/release/fractal-harness vocabulary list
