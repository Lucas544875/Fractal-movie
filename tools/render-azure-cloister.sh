#!/usr/bin/env bash
# Frozen production handoff. Run --check for a GPU-free preflight.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
mode=${1:---render}
[[ $# -le 1 && ( "$mode" == --render || "$mode" == --check ) ]] || {
  echo 'Usage: bash tools/render-azure-cloister.sh [--check|--render]' >&2; exit 2;
}
for command in python3 ffmpeg ffprobe flock timeout sha256sum pgrep; do
  command -v "$command" >/dev/null
done
python3 -c 'from PIL import Image'
sha256sum --check scenes/production/azure-cloister.sha256
python3 - <<'PY'
import shutil
if shutil.disk_usage('.').free < 5 * 1024**3:
    raise SystemExit('At least 5 GiB of free disk space is required')
PY
ffmpeg -hide_banner -encoders 2>/dev/null | { grep -q ' libx264 '; cat >/dev/null; }
ffmpeg -hide_banner -filters 2>/dev/null | { grep -q ' colorspace '; cat >/dev/null; }
if [[ "$mode" == --check ]]; then
  echo 'Preflight passed. Production render has not started.'
  exit 0
fi
mkdir -p output/azure-cloister/master-frames
exec 9>/tmp/fractal-movie-gpu.lock
flock -n 9 || { echo 'Another cooperating GPU job holds the lock.' >&2; exit 1; }
if pgrep -x 'fractal-render|fractal-harness|profile_ivory' >/dev/null; then
  echo 'An existing renderer/harness is active; finish it before starting this job.' >&2
  exit 1
fi
output=output/azure-cloister/azure-cloister.mp4
[[ ! -e "$output" ]] || { echo "Already exists: $output" >&2; exit 1; }
export WGPU_BACKEND=gl
# Include frame writing, QC and encoding in a single three-hour wall-clock budget.
# Reserve 20 seconds for termination. On timeout, valid PNGs remain resumable.
SECONDS=0
budget_run() {
  local remaining=$((10780 - SECONDS))
  (( remaining > 0 )) || { echo 'Three-hour budget exhausted.' >&2; exit 124; }
  timeout --signal=INT --kill-after=15s "${remaining}s" "$@"
}
log=output/azure-cloister/production.log
exec > >(tee -a "$log") 2>&1
date -u '+Started: %Y-%m-%dT%H:%M:%SZ'
budget_run target/release/fractal-render render scenes/production/azure-cloister.yaml \
  --adapter 'NVIDIA GeForce RTX 3070' --gpu-duty-cycle 80 \
  --output output/azure-cloister/master-frames --resume --no-video
budget_run python3 tools/check-azure-cloister.py \
  output/azure-cloister/master-frames output/azure-cloister/master-qc.json 1920 1080
budget_run bash tools/encode-sdr.sh output/azure-cloister/master-frames 30 300 "$output"
budget_run ffprobe -v error -count_frames -select_streams v:0 \
  -show_entries stream=width,height,nb_read_frames,r_frame_rate,duration,color_space,color_transfer,color_primaries,color_range \
  -of json "$output" > output/azure-cloister/video-probe.json
budget_run python3 - <<'PY'
import json
p = json.load(open('output/azure-cloister/video-probe.json'))['streams'][0]
expected = dict(width=1920, height=1080, nb_read_frames='300', r_frame_rate='30/1',
                color_space='bt709', color_transfer='bt709', color_primaries='bt709', color_range='tv')
for key, value in expected.items():
    if p.get(key) != value:
        raise SystemExit(f'Video verification failed: {key}: {p.get(key)!r} != {value!r}')
if abs(float(p['duration']) - 10.0) > 0.001:
    raise SystemExit('Video duration is not 10 seconds')
PY
echo "Completed in ${SECONDS}s: $output"
