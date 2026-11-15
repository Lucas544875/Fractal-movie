"""Decode every frame and record temporal screening metrics; no GPU needed."""
import json
import pathlib
import sys
from PIL import Image, ImageChops, ImageStat

root, destination = map(pathlib.Path, sys.argv[1:3])
dimensions = tuple(map(int, sys.argv[3:5]))
files = sorted(root.glob('frame_*.png'))
expected = [f'frame_{i:06d}.png' for i in range(300)]
if [p.name for p in files] != expected:
    raise SystemExit('Expected exactly frames 000000 through 000299')
records = []
previous = None
for i, path in enumerate(files):
    with Image.open(path) as source:
        source.load()
        if source.size != dimensions:
            raise SystemExit(f'Unexpected dimensions: {path}: {source.size}')
        gray = source.convert('RGB').convert('L')
        histogram = gray.histogram()
        pixels = sum(histogram)
        small = gray.resize((160, 90))
        record = dict(frame=i, mean_luma=ImageStat.Stat(gray).mean[0] / 255,
                      near_white_fraction=sum(histogram[253:]) / pixels,
                      near_black_fraction=sum(histogram[:3]) / pixels,
                      adjacent_difference=0.0 if previous is None else
                      ImageStat.Stat(ImageChops.difference(small, previous)).mean[0] / 255)
        records.append(record)
        previous = small
summary = dict(frame_count=len(records), dimensions=dimensions,
               mean_luma_range=[min(r['mean_luma'] for r in records), max(r['mean_luma'] for r in records)],
               max_luma_step=max(abs(a['mean_luma'] - b['mean_luma']) for a, b in zip(records, records[1:])),
               max_near_white_fraction=max(r['near_white_fraction'] for r in records),
               max_near_black_fraction=max(r['near_black_fraction'] for r in records),
               largest_difference_frames=sorted(records[1:], key=lambda r: r['adjacent_difference'], reverse=True)[:8])
destination.write_text(json.dumps(dict(summary=summary, frames=records), indent=2) + '\n')
print(json.dumps(summary, indent=2))
# Extreme all-dark/all-white frames indicate a failed shot. Other metrics are
# informational: ordinary motion also produces large adjacent differences.
if any(r['near_black_fraction'] > .99 or r['near_white_fraction'] > .99 for r in records):
    raise SystemExit('QC failed: almost entirely black or white frame')
