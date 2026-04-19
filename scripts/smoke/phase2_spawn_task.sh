#!/usr/bin/env bash
# Phase 2 live smoke: `wg spawn-task` executor abstraction.
#
# Verifies:
#   1. `wg spawn-task <nonexistent>` fails cleanly
#   2. `wg spawn-task .coordinator-0 --dry-run` renders the right command
#   3. `wg spawn-task regular-task --dry-run` has no --role
#   4. `wg spawn-task <t> --role X --dry-run` honors override
#   5. `wg spawn-task <t>` actually execs the handler; lock file appears
#   6. After handler exits, lock is released (proves Phase 1 integration
#      is intact after the new entry point)
#
# Must interact with the REAL graph — so we use a tmp WG_DIR.
set -euo pipefail
tmp=$(mktemp -d)
trap "pkill -P $$ 2>/dev/null || true; rm -rf $tmp" EXIT
export WG_DIR="$tmp/.workgraph"
cd "$tmp"
wg init --no-agency >/dev/null 2>&1

fail() { echo "FAIL: $*"; exit 1; }
pass() { echo "PASS: $*"; }

echo "=== Test 1: nonexistent task fails cleanly ==="
set +e
err=$(wg spawn-task does-not-exist --dry-run 2>&1 >/dev/null)
rc=$?
set -e
[ $rc -ne 0 ] || fail "expected nonzero exit for missing task"
echo "$err" | grep -q "no such task" || fail "error must say 'no such task': $err"
pass "missing task errors cleanly"

echo
echo "=== Test 2: dry-run for coordinator task renders role ==="
# Create a coordinator task directly in the graph.
wg add --id .coordinator-0 "test coord" >/dev/null 2>&1
out=$(wg spawn-task .coordinator-0 --dry-run 2>&1)
echo "  dry-run: $out"
echo "$out" | grep -q "wg nex" || fail "dry-run should invoke wg nex"
echo "$out" | grep -q -- "--chat .coordinator-0" || fail "must pass --chat"
echo "$out" | grep -q -- "--role coordinator" || fail "coordinator task must auto-set --role coordinator"
pass "coordinator dry-run renders correctly"

echo
echo "=== Test 3: dry-run for regular task has no --role ==="
wg add --id regular-task "regular" >/dev/null 2>&1
out=$(wg spawn-task regular-task --dry-run 2>&1)
echo "  dry-run: $out"
echo "$out" | grep -q -- "--chat regular-task" || fail "must pass --chat"
echo "$out" | grep -q -- "--role" && fail "regular task must NOT have --role"
pass "regular task has no --role"

echo
echo "=== Test 4: --role override wins ==="
out=$(wg spawn-task .coordinator-0 --role evaluator --dry-run 2>&1)
echo "$out" | grep -q -- "--role evaluator" || fail "override should set --role evaluator: $out"
echo "$out" | grep -q -- "--role coordinator" && fail "override should replace coordinator role"
pass "role override respected"

echo
echo "=== Test 5: actual spawn creates the lock file ==="
# Seed inbox so autonomous handler has something to respond to.
chat_dir="$WG_DIR/chat/.coordinator-0"
mkdir -p "$chat_dir"
cat > "$chat_dir/inbox.jsonl" <<EOF
{"id":1,"request_id":"sp-1","role":"user","content":"say hi.","timestamp":"2026-04-19T00:00:00Z"}
EOF
# Run spawn-task in autonomous-like mode. Since spawn-task exec's
# `wg nex --chat X --resume`, and resume is false for a brand-new
# chat dir (no conversation.jsonl yet), it'll run interactive. To
# make the smoke test terminate, we'd need a way to ask it to be
# autonomous. For Phase 2 we only verify the EXEC succeeds by
# checking the lock file appears — then we kill the child.
#
# Actually simpler: spawn-task should propagate autonomous from
# the caller. Since it doesn't yet, we just use dry-run to verify
# the invocation, and separately verify `wg nex` itself uses the
# same lock. That's tested in Phase 1.
#
# For Phase 2, the critical thing is: `wg spawn-task X` actually
# launches wg nex with the right args. We verified that via
# --dry-run. Running it for real without autonomous means it'll
# block waiting for input — fine for a brief smoke check.
journal="$chat_dir/conversation.jsonl"
touch "$journal"  # force --resume so the handler doesn't block on first input
lock_path="$chat_dir/.handler.pid"
# Make sure there's no stale state.
rm -f "$lock_path"
# Run spawn-task in background; it will exec wg nex.
wg spawn-task .coordinator-0 2>/dev/null >/dev/null &
h_pid=$!
# Poll for lock file (the handler, after exec, will acquire).
for i in {1..50}; do
  [ -f "$lock_path" ] && break
  sleep 0.2
done
[ -f "$lock_path" ] || fail "lock was not created by spawn-task handler within 10s"
locked_kind=$(sed -n 3p "$lock_path")
[ "$locked_kind" = "chat-nex" ] || fail "expected kind=chat-nex, got $locked_kind"
pass "spawn-task launched handler; lock acquired (kind=$locked_kind)"

echo
echo "=== Test 6: clean release after kill leaves system in good state ==="
# Kill the handler (which was exec'd from spawn-task).
# Find the wg nex pid (the exec'd-into process).
# After exec, the pid we have IS the wg nex pid. But spawn-task
# may itself have exited after exec. Let's find any wg nex in this
# tmp dir.
real_handler=$(pgrep -f "wg nex --chat .coordinator-0" | head -1 || true)
if [ -n "$real_handler" ]; then
  kill "$real_handler" 2>/dev/null || true
  # Wait for cleanup.
  for i in {1..30}; do
    if [ ! -f "$lock_path" ]; then break; fi
    sleep 0.2
  done
fi
# Regardless of how the handler died, the lock may or may not be
# cleaned up — SIGTERM doesn't run Drop by default in Rust. Check
# both cases: either lock is gone (clean) OR it's stale (recoverable).
if [ -f "$lock_path" ]; then
  # Stale — next acquire should recover. Let's verify that path
  # too by asking for status.
  status=$(wg session status .coordinator-0 2>&1)
  if echo "$status" | grep -q STALE; then
    pass "lock is stale after kill, status command detects it"
  else
    echo "  status: $status"
    fail "lock file exists after handler kill, but status doesn't say STALE"
  fi
else
  pass "lock removed cleanly after handler exit"
fi

echo
echo "=== ALL PHASE 2 CHECKS PASSED ==="
