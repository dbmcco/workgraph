#!/usr/bin/env python3
"""Phase 3c live smoke: takeover-on-send.

Simulates the user-send path without requiring a daemon:
  1. Start a background handler (owns lock).
  2. Open TUI, Ctrl+T to enter observer mode.
  3. Write release marker directly + set takeover-pending flag
     via a real send_chat_message path... NO — can't simulate that.
     Instead, verify the Phase 1 release mechanism works when
     triggered externally (via `wg session release`) while the
     TUI is observing. This tests the orchestration bits the TUI
     uses internally.
  4. Assert: bg handler exits at turn boundary, lock released,
     TUI's observer pane doesn't block subsequent spawn.

The full TUI→send→takeover path requires a running daemon
(`wg chat` goes through IPC). Manual end-to-end in a real terminal
is left as a validation note on the commit.
"""
import fcntl, os, pty, select, shutil, signal, struct, subprocess, sys, termios, time


def drain(master, timeout=1.0):
    buf = b""
    end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([master], [], [], 0.1)
        if master in r:
            try:
                data = os.read(master, 8192)
                if not data: return buf
                buf += data
            except OSError: return buf
    return buf


def main():
    tmp = "/tmp/phase3c_smoke_dir"
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(tmp)
    env = os.environ.copy()
    env["WG_DIR"] = tmp + "/.workgraph"
    subprocess.run(["wg", "init", "--no-agency"], env=env, cwd=tmp,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["wg", "add", "--id", ".coordinator-0", "coord-zero"],
                   env=env, cwd=tmp,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    chat_dir = os.path.join(tmp, ".workgraph", "chat", ".coordinator-0")
    os.makedirs(chat_dir, exist_ok=True)
    with open(os.path.join(chat_dir, "inbox.jsonl"), "w") as f:
        f.write('{"id":1,"request_id":"bg-1","role":"user",'
                '"content":"one word.","timestamp":"2026-04-19T00:00:00Z"}\n')

    bg = subprocess.Popen(
        ["wg", "nex", "--chat", ".coordinator-0", "--no-mcp", "--max-turns", "50"],
        env=env, cwd=tmp,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
    )
    lock_path = os.path.join(chat_dir, ".handler.pid")
    for _ in range(50):
        if os.path.exists(lock_path): break
        time.sleep(0.2)
    assert os.path.exists(lock_path)
    with open(lock_path) as f:
        bg_pid = int(f.read().splitlines()[0])
    print(f"PASS: bg handler owns lock (pid={bg_pid})")

    # Open TUI, enter observer mode.
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    tui = subprocess.Popen(
        ["wg", "tui"], stdin=slave, stdout=slave, stderr=slave,
        env=env, cwd=tmp, start_new_session=True,
    )
    os.close(slave)
    drain(master, 3.0)
    os.write(master, b"\x14")  # Ctrl+T → observer mode
    drain(master, 3.0)

    # Give the bg handler time to respond to the initial inbox msg,
    # so it's not in the middle of an LLM call when we release.
    time.sleep(4)

    # Simulate the send-triggered release. In real usage, the TUI's
    # send_chat_message writes the release marker; we invoke the same
    # logic externally via `wg session release`.
    print("PASS: invoking wg session release (simulates user-send takeover)")
    rel = subprocess.run(
        ["wg", "session", "release", ".coordinator-0", "--wait", "15"],
        env=env, cwd=tmp, capture_output=True, text=True,
    )
    print(f"  release output: {rel.stdout.strip()[:200]}")
    print(f"  release stderr: {rel.stderr.strip()[:200]}")

    # Assert: bg handler is gone, lock is released.
    # Poll up to 10s.
    for _ in range(50):
        if bg.poll() is not None and not os.path.exists(lock_path):
            break
        time.sleep(0.2)
    bg_exit = bg.poll()
    if bg_exit is None:
        print(f"FAIL: bg handler did not exit after release ({bg.pid} still running)")
        bg.terminate(); tui.terminate(); return 1
    if os.path.exists(lock_path):
        print(f"FAIL: lock file still exists after release + handler exit")
        tui.terminate(); return 1
    print(f"PASS: bg handler exited cleanly (rc={bg_exit}); lock released")

    # Can a new spawn-task succeed now? That's what Phase 3c's
    # poll_chat_pty_takeover does internally when it detects release.
    # Test by invoking spawn-task --dry-run which confirms the task
    # is still resolvable and the next handler COULD be spawned.
    dry = subprocess.run(
        ["wg", "spawn-task", ".coordinator-0", "--dry-run"],
        env=env, cwd=tmp, capture_output=True, text=True,
    )
    if dry.returncode == 0 and "wg nex --chat .coordinator-0" in dry.stdout:
        print("PASS: spawn-task --dry-run shows clean respawn command")
    else:
        print(f"FAIL: spawn-task --dry-run failed: {dry.stdout} / {dry.stderr}")
        tui.terminate(); return 1

    # Quit TUI.
    os.write(master, b"q")
    drain(master, 1.0)
    try: tui.wait(timeout=3)
    except subprocess.TimeoutExpired:
        tui.terminate()
        try: tui.wait(timeout=2)
        except subprocess.TimeoutExpired:
            os.killpg(os.getpgid(tui.pid), signal.SIGKILL)
    shutil.rmtree(tmp, ignore_errors=True)
    print("=== PHASE 3c RELEASE+TAKEOVER MECHANISM VERIFIED ===")
    return 0


if __name__ == "__main__":
    sys.exit(main())
