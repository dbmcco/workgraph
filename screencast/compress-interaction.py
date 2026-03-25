#!/usr/bin/env python3
"""Compress interaction-raw.cast into a 45-60s time-lapse with scene markers.

Scene-aware compression tuned for the interaction screencast:
- Scene 0 (CLI): fast typing (3×), output waits short
- Scene 1 (Launch): fast typing (3×)
- Scene 2 (Chat): fast typing (3×), coordinator response SLOWED DOWN to be visible
- Scene 3 (Agents): moderate — keep status transitions visible
- Scene 4 (Detail View): moderate — live output is the payoff
- Scene 5 (Round 2): fast typing (3×), coordinator moderate
- Scene 6 (Results): slow — let viewer read haiku output
- Scene 7 (Survey + Exit): moderate navigation, clean exit
"""

import json
import re
import sys

RAW_PATH = "screencast/recordings/interaction-raw.cast"
COMPRESSED_PATH = "screencast/recordings/interaction-compressed.cast"
TIMEMAP_PATH = "screencast/recordings/interaction-timemap.json"

# Scene transition pause
SCENE_PAUSE = 0.5

# Per-scene compression: (interaction_speed, activity_speed, short_wait_cap, long_wait_cap)
#   interaction_speed: divides gaps <0.3s (typing/keystrokes). Higher = faster typing.
#   activity_speed: divides gaps 0.3-1s (TUI updating). Higher = faster updates.
#   short_wait_cap: cap for gaps 1-3s (coordinator think-time, agent waits)
#   long_wait_cap: cap for gaps >3s (long pauses between actions)
SCENE_PARAMS = {
    # Scene 0 (~3-4s target): CLI — typing 3× faster, output waits short
    "cli":         (3.0, 2.0, 0.15, 0.10),
    # Scene 1 (~2-3s target): typing 3× faster
    "launch":      (3.0, 2.0, 0.20, 0.15),
    # Scene 2 (~4-6s target): typing 3×, but SLOW DOWN coordinator response
    "chat":        (3.0, 1.2, 0.40, 0.30),
    # Scene 3 (~6-8s target): moderate — keep status transitions visible
    "agents":      (1.5, 1.5, 0.25, 0.20),
    # Scene 4 (~12-15s target): near real-speed for live output (the payoff)
    "detail":      (1.0, 1.5, 0.40, 0.35),
    # Scene 5 (~4-6s target): typing 3×, coordinator moderate
    "round2":      (3.0, 1.5, 0.30, 0.20),
    # Scene 6 (~6-8s target): results — slow enough to read haiku output
    "results":     (1.0, 1.5, 0.50, 0.40),
    # Scene 7 (~3-4s target): final survey navigation + exit
    "survey":      (1.5, 2.0, 0.20, 0.15),
    # Exit — clean
    "exit":        (1.0, 2.0, 0.20, 0.15),
}

DEFAULT_PARAMS = (1.5, 3.0, 0.20, 0.15)


SCENES_OVERRIDE_PATH = "screencast/recordings/interaction-scenes.json"


def load_cast(path):
    with open(path, "r") as f:
        lines = f.readlines()
    header = json.loads(lines[0])
    frames = [json.loads(line) for line in lines[1:]]
    return header, frames, lines


def load_scene_overrides(frames):
    """Load explicit scene boundaries from JSON override file.

    File format: {"cli": 0.0, "launch": 7.6, "chat": 10.0, ...}
    Values are timestamps in seconds. Converted to frame indices.

    Returns list of (scene_name, start_frame_idx) or None if no file.
    """
    import os
    if not os.path.exists(SCENES_OVERRIDE_PATH):
        return None

    with open(SCENES_OVERRIDE_PATH) as f:
        overrides = json.load(f)

    scenes = []
    for scene_name, start_time in overrides.items():
        # Find the frame closest to this timestamp
        best_idx = 0
        best_diff = float("inf")
        for i, frame in enumerate(frames):
            diff = abs(frame[0] - start_time)
            if diff < best_diff:
                best_diff = diff
                best_idx = i
        scenes.append((scene_name, best_idx))

    scenes.sort(key=lambda x: x[1])
    return scenes


def detect_scenes(frames):
    """Detect scene boundaries from content.

    Returns list of (scene_name, start_frame_idx).
    """
    scenes = [("cli", 0)]
    launch_started = False
    tui_entered = False
    chat_started = False
    agents_started = False
    detail_started = False
    round2_started = False
    results_started = False
    survey_started = False

    # Track previous task count for detecting jumps
    prev_task_count = 0
    agents_start_frame = 0
    round2_start_frame = 0

    for i, frame in enumerate(frames):
        clean = re.sub(r'\x1b\[[^a-zA-Z]*[a-zA-Z]', '', frame[2])

        # Detect "wg tui" command typed — transition from CLI to launch scene
        if not launch_started and "wg tui" in clean:
            launch_started = True
            scenes.append(("launch", i))
            continue

        # TUI loaded — transition to chat scene
        if not tui_entered and ("Chat" in clean or "LIVE" in clean):
            # Look for task count in status bar
            if re.search(r'\d+ tasks', clean):
                tui_entered = True
                scenes.append(("chat", i))
                continue

        if not tui_entered:
            continue

        # Chat input submitted (detect coordinator response or task count jump)
        m = re.search(r'(\d+) tasks \((\d+) done', clean)
        if m:
            task_count = int(m.group(1))

            # Agent spawn phase: tasks jump to 5+
            if not agents_started and task_count >= 5:
                agents_started = True
                agents_start_frame = i
                scenes.append(("agents", i))

            # Detail view phase: detect ACTIVE tab (not just header).
            # Wait at least 10 frames after agents for the user to navigate there.
            if not detail_started and agents_started and i >= agents_start_frame + 10:
                # Look for tab being actively shown (Detail content, Agency, Firehose)
                if ("Status" in clean and "Description" in clean) or \
                   "Agency" in clean or "Fire" in clean:
                    detail_started = True
                    scenes.append(("detail", i))

            # Round 2: task count jumps again (roast tasks added)
            if not round2_started and task_count >= 9:
                # Check if roast-related content visible
                if "snark" in clean.lower() or "roast" in clean.lower() or task_count >= 10:
                    round2_started = True
                    round2_start_frame = i
                    # Look back to find typing start
                    typing_start = i
                    for j in range(max(0, i - 30), i):
                        gap = frames[j][0] - frames[j - 1][0] if j > 0 else 0
                        if gap < 0.25:
                            typing_start = j
                            break
                    scenes.append(("round2", typing_start))

            prev_task_count = task_count

        # Results reveal: after round2, detect when draft-haikus Log tab is shown
        # (the user navigates to draft-haikus and presses 2 for Log tab)
        if round2_started and not results_started and i > round2_start_frame + 20:
            # Look for "draft-haiku" appearing in inspector area with Log tab active
            if "draft-haiku" in clean.lower() and "2:Log" in clean:
                results_started = True
                scenes.append(("results", i))

        # Survey/exit: after results (or round2 if no results), detect fast navigation
        if (results_started or round2_started) and not survey_started:
            # Survey starts when we see rapid upward navigation after results
            if results_started and i > 0:
                # Detect consecutive fast frames (rapid arrow key navigation)
                if i > 0 and (frames[i][0] - frames[i-1][0]) < 0.5:
                    # Only after results scene has had some time
                    result_scene_idx = next((idx for name, idx in scenes if name == "results"), 0)
                    if i > result_scene_idx + 30:
                        survey_started = True
                        scenes.append(("survey", i))
            elif not results_started and i > round2_start_frame + 50:
                if i > 0 and (frames[i][0] - frames[i-1][0]) < 0.5:
                    survey_started = True
                    scenes.append(("survey", i))

    # If we didn't detect all scenes, add reasonable defaults
    if not launch_started:
        scenes.append(("launch", len(frames) // 10))
    if not agents_started:
        scenes.append(("agents", len(frames) // 4))
    if not detail_started:
        scenes.append(("detail", len(frames) // 3))
    if not round2_started:
        scenes.append(("round2", len(frames) // 2))
    if not results_started:
        scenes.append(("results", 3 * len(frames) // 4))
    if not survey_started:
        scenes.append(("survey", 7 * len(frames) // 8))

    # Sort by frame index
    scenes.sort(key=lambda x: x[1])
    return scenes


def get_scene_at(frame_idx, scenes):
    """Return the scene name for a given frame index."""
    current = "launch"
    for name, start_idx in scenes:
        if frame_idx >= start_idx:
            current = name
        else:
            break
    return current


def classify_and_compress(frames, scenes):
    """Compute compressed timestamps with scene-aware compression."""
    if not frames:
        return [], [], {}

    scene_transitions = {idx for _, idx in scenes}

    compressed_time = 0.0
    timemap = [{"compressed_s": 0.0, "real_s": round(frames[0][0], 6)}]
    new_timestamps = [0.0]

    for i in range(1, len(frames)):
        real_gap = frames[i][0] - frames[i - 1][0]
        current_scene = get_scene_at(i, scenes)

        # Scene transition pause
        if i in scene_transitions:
            compressed_time += SCENE_PAUSE

        interaction_speed, activity_speed, short_wait_cap, long_wait_cap = \
            SCENE_PARAMS.get(current_scene, DEFAULT_PARAMS)

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

    # Per-scene durations
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
    """Write compressed cast file preserving exact frame content."""
    with open(out_path, "w", newline="") as f:
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
    scenes = load_scene_overrides(frames)
    if scenes:
        print(f"  Using explicit scene overrides from {SCENES_OVERRIDE_PATH}")
    else:
        scenes = detect_scenes(frames)
        print("  Using auto-detected scenes")
    for name, idx in scenes:
        print(f"  {name}: frame {idx}, t={frames[idx][0]:.1f}s")

    print("\nCompressing...")
    new_timestamps, timemap, scene_durations = classify_and_compress(frames, scenes)

    compressed_duration = new_timestamps[-1] - new_timestamps[0]
    ratio = raw_duration / compressed_duration if compressed_duration > 0 else 0
    print(f"  Compressed duration: {compressed_duration:.1f}s")
    print(f"  Compression ratio: {ratio:.1f}x")

    print("\nPer-scene durations:")
    for name, (real_dur, comp_dur) in scene_durations.items():
        scene_ratio = real_dur / comp_dur if comp_dur > 0 else float("inf")
        print(f"  {name:15s}: {real_dur:7.1f}s -> {comp_dur:5.1f}s ({scene_ratio:.1f}x)")

    if compressed_duration < 45 or compressed_duration > 75:
        print(f"\n  WARNING: Duration {compressed_duration:.1f}s outside target 45-60s range")

    print(f"\nWriting {COMPRESSED_PATH}...")
    write_compressed(header, frames, new_timestamps, raw_lines, COMPRESSED_PATH)

    print(f"Writing {TIMEMAP_PATH}...")
    with open(TIMEMAP_PATH, "w") as f:
        json.dump(timemap, f, indent=2)

    # Validation
    print("\nValidation:")

    with open(COMPRESSED_PATH) as f:
        comp_header = json.loads(f.readline())
    assert comp_header["width"] == 65, f"Width changed: {comp_header['width']}"
    assert comp_header["height"] == 38, f"Height changed: {comp_header['height']}"
    print(f"  Dimensions preserved: {comp_header['width']}x{comp_header['height']}")

    with open(COMPRESSED_PATH) as f:
        comp_lines = f.readlines()
    comp_frames = [json.loads(line) for line in comp_lines[1:]]

    max_gap = 0
    for i in range(1, len(comp_frames)):
        gap = comp_frames[i][0] - comp_frames[i - 1][0]
        if gap > max_gap:
            max_gap = gap
    print(f"  Max gap in compressed: {max_gap:.2f}s")

    assert len(comp_frames) == len(frames), "Frame count mismatch"
    print(f"  Frame count preserved: {len(comp_frames)}")

    for i in range(len(frames)):
        assert comp_frames[i][1] == frames[i][1], f"Frame {i} type differs"
        assert comp_frames[i][2] == frames[i][2], f"Frame {i} content differs"
    print(f"  All frame content identical")

    assert len(timemap) == len(frames), "Timemap size mismatch"
    print(f"  Timemap has {len(timemap)} entries")

    crlf_count = sum(1 for f in comp_frames if "\r\n" in f[2])
    print(f"  CR+LF frames: {crlf_count}/{len(comp_frames)}")

    print(f"\nDone! {raw_duration:.1f}s -> {compressed_duration:.1f}s ({ratio:.1f}x compression)")


if __name__ == "__main__":
    main()
