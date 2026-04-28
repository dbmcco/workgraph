#!/bin/bash
# Verify screencast-graph-nav.cast meets all criteria
set -e

CAST="screencast/recordings/screencast-graph-nav.cast"

if [ ! -f "$CAST" ]; then
    echo "FAIL: $CAST not found"
    exit 1
fi

python3 -c "
import json, sys

with open('$CAST') as f:
    lines = f.readlines()

header = json.loads(lines[0])
frames = [json.loads(line) for line in lines[1:]]
errors = []

# Valid .cast file
if header.get('version') != 2:
    errors.append('not asciinema v2')
if len(frames) < 10:
    errors.append(f'too few frames: {len(frames)}')

# Duration 10-20s
dur = frames[-1][0]
if dur < 10 or dur > 20:
    errors.append(f'duration {dur:.1f}s outside 10-20s')

# Tab switching
tab_seen = any('Tab' in f[2] for f in frames)
if not tab_seen:
    errors.append('no Tab key indicator')

# Arrow navigation
arrow_down = any(chr(0x2193) in f[2] for f in frames)  # ↓
arrow_up = any(chr(0x2191) in f[2] for f in frames)    # ↑
if not arrow_down:
    errors.append('no ↓ arrow key')
if not arrow_up:
    errors.append('no ↑ arrow key')

# All 4 detail panes
for tab in ['Detail', 'Log', 'Msg', 'Agency']:
    if not any(tab in f[2] for f in frames):
        errors.append(f'missing {tab} pane')

if errors:
    print('FAIL:', '; '.join(errors))
    sys.exit(1)

print(f'OK: valid .cast, {dur:.1f}s, {len(frames)} frames, Tab + arrows + 4 detail panes')
"
