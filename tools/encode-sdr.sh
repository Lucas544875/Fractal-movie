#!/usr/bin/env bash
# Encode the renderer's sRGB PNG master to a tagged BT.709 SDR MP4.
# The first scale makes the RGB -> full-range YUV matrix explicit; the
# colorspace filter then converts the transfer curve and output range.
# https://ffmpeg.org/ffmpeg-filters.html#colorspace
set -euo pipefail
if [ "$#" -ne 4 ]; then
  echo "Usage: bash tools/encode-sdr.sh FRAMES_DIRECTORY FPS FRAME_COUNT OUTPUT.mp4" >&2
  exit 2
fi
frames_directory=$1
frame_rate=$2
frame_count=$3
output_path=$4
[[ "$frame_rate" =~ ^[1-9][0-9]*$ && "$frame_count" =~ ^[1-9][0-9]*$ ]] || { echo 'FPS and frame count must be positive integers' >&2; exit 2; }
[[ "$output_path" == *.mp4 && ! -e "$output_path" ]] || { echo 'Output must be a new .mp4 file' >&2; exit 2; }
for (( index=0; index<frame_count; index++ )); do
  printf -v frame_path '%s/frame_%06d.png' "$frames_directory" "$index"
  [[ -f "$frame_path" ]] || { echo "Missing frame: $frame_path" >&2; exit 1; }
done
# Unique sibling temporary keeps interrupted encodes out of the deliverables.
temporary_path=$(mktemp "${output_path%.mp4}.XXXXXX.mp4")
trap 'rm -f -- "$temporary_path"' EXIT
ffmpeg -hide_banner -y -framerate "$frame_rate" -start_number 0 \
  -i "$frames_directory/frame_%06d.png" -frames:v "$frame_count" -an \
  -vf 'scale=in_range=pc:out_range=pc:out_color_matrix=bt709,format=yuv444p10le,colorspace=ispace=bt709:iprimaries=bt709:itrc=srgb:irange=pc:all=bt709:range=tv:format=yuv420p:dither=fsb' \
  -c:v libx264 -preset slow -crf 14 -pix_fmt yuv420p \
  -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv \
  -movflags +faststart "$temporary_path"
# Publish without replacing a pre-existing result, even if another process
# created the output during encoding. Hard-link publication is atomic.
ln -- "$temporary_path" "$output_path"
