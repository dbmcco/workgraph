#!/usr/bin/env bash
# setup-demo.sh — Create a clean demo project for hero screencast recording.
# Usage: ./setup-demo.sh [demo-dir]
#
# Creates a fresh wg project in a temp directory (or specified path),
# configures it for fast, visual demos, and prints next steps.

set -euo pipefail

DEMO_DIR="${1:-/tmp/wg-hero-demo}"

echo "=== Workgraph Hero Screencast Setup ==="
echo ""

# Clean slate
if [ -d "$DEMO_DIR" ]; then
    echo "Removing existing demo directory: $DEMO_DIR"
    rm -rf "$DEMO_DIR"
fi

mkdir -p "$DEMO_DIR"
cd "$DEMO_DIR"

# Initialize git repo (wg needs one)
git init -q
git commit --allow-empty -m "init" -q

# Initialize workgraph project
wg init

# Configure for demo: fast agents, sonnet model
wg config --max-agents 4
wg config --model sonnet
wg config --coordinator-executor claude

echo ""
echo "Demo project initialized at: $DEMO_DIR"
echo ""
echo "Next steps:"
echo "  cd $DEMO_DIR"
echo "  ./record.sh heist    # Record Heist Movie Night scenario"
echo "  ./record.sh haiku    # Record Haiku Pipeline scenario"
echo "  ./record.sh pancakes # Record Debug Pancakes scenario"
echo ""
echo "Or manually:"
echo "  asciinema rec --idle-time-limit 2 screencast.cast"
echo "  wg tui"
echo "  # Type your scenario prompt in the coordinator chat"
echo "  # Wait for tasks to complete"
echo "  # Ctrl-C to exit TUI, then Ctrl-D to stop recording"
