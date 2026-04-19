#!/usr/bin/env python3
"""Phase 3a live smoke: Ctrl+T in wg tui toggles PTY mode for Chat tab.

Drives `wg tui` through a real PTY, sends keys (switch to Chat tab,
then Ctrl+T to enable PTY mode), and observes the screen for proof
that an embedded `wg nex` rendered in the messages area.
"""
import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import termios
import time


def send(master, s):
    os.write(master, s.encode() if isinstance(s, str) else s)


def drain(master, timeout=1.0):
    """Read everything available up to timeout."""
    buf = b""
    end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([master], [], [], 0.1)
        if master in r:
            try:
                data = os.read(master, 8192)
                if not data:
                    return buf
                buf += data
            except OSError:
                return buf
    return buf


def main():
    tmp = "/tmp/phase3a_smoke_dir"
    import shutil
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(tmp, exist_ok=True)  # tmp dir, NOT .workgraph — let init create it
    env = os.environ.copy()
    env["WG_DIR"] = tmp + "/.workgraph"
    # init + create a coordinator task
    subprocess.run(
        ["wg", "init", "--no-agency"], env=env, cwd=tmp,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    subprocess.run(
        ["wg", "add", "--id", ".coordinator-0", "test-coord"],
        env=env, cwd=tmp,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )

    master, slave = pty.openpty()
    # 40x120 — plenty of room for PTY inside
    winsize = struct.pack("HHHH", 40, 120, 0, 0)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, winsize)
    proc = subprocess.Popen(
        ["wg", "tui"],
        stdin=slave, stdout=slave, stderr=slave,
        env=env, cwd=tmp, start_new_session=True,
    )
    os.close(slave)

    # Wait for the TUI to settle (give it 3s).
    drain(master, 3.0)

    # Ensure we're on the Chat tab (tab index 0 — default).
    # Toggle PTY mode with Ctrl+T.
    send(master, b"\x14")  # Ctrl+T

    # Let the pane spawn + do initial render.
    out = drain(master, 10.0)

    # Success criteria: somewhere in the captured output, we should
    # see evidence that wg nex ran inside the embedded PTY — its
    # banner has "wg nex" text. The outer TUI's rendering may
    # interleave escape sequences, so we search the decoded bytes
    # loosely.
    flat = out.decode(errors="replace")
    found_banner = "wg nex" in flat
    # Also check that a lock file was created for the embedded handler.
    lock_path = os.path.join(
        tmp, ".workgraph", "chat", ".coordinator-0", ".handler.pid"
    )
    has_lock = os.path.exists(lock_path)

    print("=== Phase 3a: Ctrl+T toggles PTY mode in Chat tab ===")
    print(f"  wg nex banner in TUI output: {found_banner}")
    print(f"  .handler.pid exists at {lock_path}: {has_lock}")

    # Quit the TUI cleanly. Use 'q' — the standard wg tui quit key.
    send(master, b"q")
    drain(master, 2.0)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
    # Let lock cleanup settle.
    time.sleep(1.0)

    # Clean up the tmp dir's chat state for a deterministic next run.
    import shutil
    shutil.rmtree(tmp, ignore_errors=True)

    if found_banner and has_lock:
        print("PASS: PTY mode spawned a handler and rendered its banner")
        return 0
    print("FAIL: expected both banner AND lock to appear")
    return 1


if __name__ == "__main__":
    sys.exit(main())
