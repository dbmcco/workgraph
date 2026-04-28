#!/usr/bin/env python3
"""Compress showcase-raw.cast into a 60-90s time-lapse with scene markers.

Strategy (same family as compress-hero-v3.py but tuned for multi-scene):
- Interaction moments (gaps <0.3s): ~1x speed (typing, navigation)
- Activity bursts (gaps 0.3-1s): ~2x speed (TUI updating, status changes)
- Short waits (gaps 1-3s): compress to 0.5s cap
- Long waits (gaps >3s): compress to 0.3-0.4s
- Scene transitions: inject 0.5s pause between major scenes

Scenes (detected from content):
1. Launch: shell → wg viz → wg service start → wg tui
2. Fan-out: agents claim tasks, parallel work visible
3. Inspection: tab switches (Detail/Log/Output), navigation
4. Completions: pipeline finishing, tasks going done
5. Coordinator: chat tab, typing message, coordinator response
6. Second wave: new roast tasks dispatched and completing
"""

import json
import re
import sys

RAW_PATH = "screencast/recordings/showcase-raw.cast"
COMPRESSED_PATH = "screencast/recordings/showcase-compressed.cast"
TIMEMAP_PATH = "screencast/recordings/showcase-timemap.json"

# Scene transition pause
SCENE_PAUSE = 0.5

# Per-scene compression parameters: (interaction_speed, activity_speed, short_wait_cap, long_wait_cap)
# Tuned to hit design-doc target durations per scene.
SCENE_PARAMS = {
    # Scene 1 (15.8s → ~5s): Compress everything ~3x
    "launch":      (2.0, 3.0, 0.25, 0.20),
    # Scene 2 (116.8s → ~15s): Heavy on waits, moderate on interaction
    "fanout":      (1.5, 3.0, 0.13, 0.15),
    # Scene 3 (136.4s → ~15s): TUI refreshes are interaction-speed, waits crushed
    "inspection":  (1.8, 3.5, 0.18, 0.08),
    # Scene 4 (99.3s → ~10s): Very aggressive on everything
    "completions": (3.0, 5.0, 0.10, 0.06),
    # Scene 5 (7.8s → ~8-10s): Keep natural — this is the typing moment
    "coordinator": (1.0, 1.5, 0.40, 0.30),
    # Scene 6 (617.6s → ~15s): Ultra aggressive on massive waits
    "second_wave": (2.0, 4.0, 0.08, 0.06),
    # Exit (short)
    "exit":        (1.0, 2.0, 0.30, 0.20),
}

# Default for any scene not in the map
DEFAULT_PARAMS = (1.5, 3.0, 0.20, 0.15)


def load_cast(path):
    with open(path, "r") as f:
        lines = f.readlines()
    header = json.loads(lines[0])
    frames = [json.loads(line) for line in lines[1:]]
    return header, frames, lines


def detect_scenes(frames):
    """Detect scene boundaries from frame content.

    Returns a list of (scene_name, start_frame_idx) tuples.
    """
    scenes = []
    prev_task_count = 0
    prev_done_count = 0
    tui_entered = False
    chat_phase = False
    second_wave = False
    inspection_started = False
    completions_started = False

    for i, frame in enumerate(frames):
        t = frame[0]
        clean = re.sub(r'\x1b\[[^a-zA-Z]*[a-zA-Z]', '', frame[2])

        # Scene 1: Launch (from start until TUI renders)
        if not tui_entered:
            m = re.search(r'(\d+) tasks \((\d+) done', clean)
            if m:
                tui_entered = True
                scenes.append(("launch", 0))
                scenes.append(("fanout", i))
                prev_task_count = int(m.group(1))
                prev_done_count = int(m.group(2))
            continue

        # Track status bar
        m = re.search(r'(\d+) tasks \((\d+) done, (\d+) open, (\d+) active\)', clean)
        if m:
            task_count = int(m.group(1))
            done_count = int(m.group(2))
            active_count = int(m.group(4))

            # Scene 3: Inspection — starts when user begins navigating
            # (detected by done count reaching ~7 and frame gaps suggest navigation)
            if not inspection_started and done_count >= 7:
                inspection_started = True
                scenes.append(("inspection", i))

            # Scene 4: Completions — pipeline tasks finishing
            if not completions_started and done_count >= 11:
                completions_started = True
                scenes.append(("completions", i))

            # Scene 5: Coordinator — task count jumps (coordinator adds tasks)
            if not chat_phase and task_count > prev_task_count + 2 and task_count >= 30:
                chat_phase = True
                # Look back to find the typing cluster start
                # The chat typing starts a few frames before the task count jump
                typing_start = i
                for j in range(max(0, i - 40), i):
                    gap = frames[j][0] - frames[j - 1][0] if j > 0 else 0
                    if gap < 0.25 and j > 0:
                        typing_start = j
                        break
                scenes.append(("coordinator", typing_start))

            # Scene 6: Second wave — after coordinator tasks are created
            if chat_phase and not second_wave and task_count >= 40:
                second_wave = True
                scenes.append(("second_wave", i))

            prev_task_count = task_count
            prev_done_count = done_count

    # Find exit (last frames with wg viz or q press)
    for i in range(len(frames) - 1, max(0, len(frames) - 20), -1):
        clean = re.sub(r'\x1b\[[^a-zA-Z]*[a-zA-Z]', '', frames[i][2])
        if 'wg viz' in clean or '$ ' in clean:
            scenes.append(("exit", i))
            break

    return scenes


def get_scene_at(frame_idx, scenes):
    """Return the scene name for a given frame index."""
    current_scene = "unknown"
    for name, start_idx in scenes:
        if frame_idx >= start_idx:
            current_scene = name
        else:
            break
    return current_scene


def classify_and_compress(frames, scenes):
    """Compute compressed timestamps with scene-aware compression."""
    if not frames:
        return [], [], {}

    # Build set of scene transition frame indices
    scene_transitions = set()
    for _, start_idx in scenes:
        scene_transitions.add(start_idx)

    compressed_time = 0.0
    timemap = [{"compressed_s": 0.0, "real_s": round(frames[0][0], 6)}]
    new_timestamps = [0.0]

    for i in range(1, len(frames)):
        real_gap = frames[i][0] - frames[i - 1][0]
        current_scene = get_scene_at(i, scenes)

        # Add scene transition pause at scene boundaries
        if i in scene_transitions:
            compressed_time += SCENE_PAUSE

        # Get per-scene compression parameters
        interaction_speed, activity_speed, short_wait_cap, long_wait_cap = \
            SCENE_PARAMS.get(current_scene, DEFAULT_PARAMS)

        # Classify and compress the gap
        if real_gap > 3.0:
            compressed_gap = long_wait_cap
        elif real_gap > 1.0:
            compressed_gap = short_wait_cap
        elif real_gap > 0.3:
            compressed_gap = real_gap / activity_speed
        else:
            compressed_gap = real_gap / interaction_speed

        compressed_time += compressed_gap
        new_timestamps.append(compressed_time)
        timemap.append({
            "compressed_s": round(compressed_time, 6),
            "real_s": round(frames[i][0], 6),
        })

    # Compute per-scene durations
    scene_list = [(name, idx) for name, idx in scenes]
    scene_durations = {}
    for si in range(len(scene_list)):
        name, start_idx = scene_list[si]
        end_idx = scene_list[si + 1][1] if si + 1 < len(scene_list) else len(frames) - 1
        real_dur = frames[end_idx][0] - frames[start_idx][0]
        comp_dur = new_timestamps[end_idx] - new_timestamps[start_idx]
        scene_durations[name] = (real_dur, comp_dur)

    return new_timestamps, timemap, scene_durations


def write_compressed(header, frames, new_timestamps, raw_lines, out_path):
    """Write compressed cast file preserving exact frame content and CR+LF."""
    with open(out_path, "w", newline="") as f:
        # Write header exactly as-is
        header_line = raw_lines[0]
        f.write(header_line)
        if not header_line.endswith("\n"):
            f.write("\n")

        for i, frame in enumerate(frames):
            new_frame = [round(new_timestamps[i], 6), frame[1], frame[2]]
            line = json.dumps(new_frame, ensure_ascii=False)
            f.write(line + "\n")


def main():
    print(f"Loading {RAW_PATH}...")
    header, frames, raw_lines = load_cast(RAW_PATH)

    print(f"  Header: {header['width']}x{header['height']}, {len(frames)} frames")
    raw_duration = frames[-1][0] - frames[0][0]
    print(f"  Raw duration: {raw_duration:.1f}s ({raw_duration/60:.1f} min)")

    print("\nDetecting scenes...")
    scenes = detect_scenes(frames)
    for name, idx in scenes:
        print(f"  {name}: frame {idx}, t={frames[idx][0]:.1f}s")

    print("\nCompressing...")
    new_timestamps, timemap, scene_durations = classify_and_compress(frames, scenes)

    compressed_duration = new_timestamps[-1] - new_timestamps[0]
    ratio = raw_duration / compressed_duration
    print(f"  Compressed duration: {compressed_duration:.1f}s")
    print(f"  Compression ratio: {ratio:.1f}x")

    print("\nPer-scene durations:")
    for name, (real_dur, comp_dur) in scene_durations.items():
        scene_ratio = real_dur / comp_dur if comp_dur > 0 else float("inf")
        print(f"  {name:15s}: {real_dur:7.1f}s → {comp_dur:5.1f}s ({scene_ratio:.1f}x)")

    if compressed_duration < 60 or compressed_duration > 90:
        print(f"\n  WARNING: Duration {compressed_duration:.1f}s outside target 60-90s range")

    print(f"\nWriting {COMPRESSED_PATH}...")
    write_compressed(header, frames, new_timestamps, raw_lines, COMPRESSED_PATH)

    print(f"Writing {TIMEMAP_PATH}...")
    with open(TIMEMAP_PATH, "w") as f:
        json.dump(timemap, f, indent=2)

    # ── Validation ──
    print("\nValidation:")

    # Dimensions preserved
    with open(COMPRESSED_PATH) as f:
        comp_header = json.loads(f.readline())
    assert comp_header["width"] == 65, f"Width changed: {comp_header['width']}"
    assert comp_header["height"] == 38, f"Height changed: {comp_header['height']}"
    print(f"  ✓ Dimensions preserved: {comp_header['width']}x{comp_header['height']}")

    # Max gap
    with open(COMPRESSED_PATH) as f:
        comp_lines = f.readlines()
    comp_frames = [json.loads(line) for line in comp_lines[1:]]
    max_gap = 0
    for i in range(1, len(comp_frames)):
        gap = comp_frames[i][0] - comp_frames[i - 1][0]
        if gap > max_gap:
            max_gap = gap
    print(f"  ✓ Max gap in compressed: {max_gap:.2f}s")

    # Frame count preserved
    assert len(comp_frames) == len(frames), "Frame count mismatch"
    print(f"  ✓ Frame count preserved: {len(comp_frames)}")

    # Content identical
    for i in range(len(frames)):
        assert comp_frames[i][1] == frames[i][1], f"Frame {i} type differs"
        assert comp_frames[i][2] == frames[i][2], f"Frame {i} content differs"
    print(f"  ✓ All frame content identical")

    # Timemap size
    assert len(timemap) == len(frames), "Timemap size mismatch"
    print(f"  ✓ Timemap has {len(timemap)} entries")

    # CR+LF preserved
    crlf_count = sum(1 for f in comp_frames if "\r\n" in f[2])
    print(f"  ✓ CR+LF frames: {crlf_count}/{len(comp_frames)}")

    print(f"\nDone! {raw_duration:.1f}s → {compressed_duration:.1f}s ({ratio:.1f}x compression)")


if __name__ == "__main__":
    main()
