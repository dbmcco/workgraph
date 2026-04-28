#!/usr/bin/env bash
#
# Minimal SWE-bench-shaped smoke test for `wg nex --eval-mode`.
#
# Simulates what a real benchmark harness does:
#   1. Check out a repo at a known state
#   2. Establish a broken test that currently fails
#   3. Hand the agent a task description, invoke it non-interactively
#   4. After the agent exits, run the test to see if the fix landed
#   5. Parse the agent's JSON summary from stdout for telemetry
#
# If this script exits 0, the design holds end-to-end: the binary is
# invokable by a harness, it works in a specified cwd without
# polluting the repo surface, and the JSON summary is parseable.
#
# Real-world extension: swap the toy test for a SWE-bench instance's
# failing test + replay the instance's pre-patch repo state.
#
# Requires: jq (for JSON parsing), a working WG_MODEL + provider.

set -euo pipefail

tmp=$(mktemp -d)
trap "rm -rf $tmp" EXIT

cd "$tmp"
git init -q
git -c user.name=eval -c user.email=eval@eval config commit.gpgsign false >/dev/null 2>&1 || true

cat > greet.sh <<'EOF'
#!/usr/bin/env bash
echo "hello world"
EOF
chmod +x greet.sh
cat > run_test.sh <<'EOF'
#!/usr/bin/env bash
./greet.sh | grep -q "hello moon"
EOF
chmod +x run_test.sh
git add greet.sh run_test.sh
git -c user.name=eval -c user.email=eval@eval commit -q -m "initial: failing test"

echo "=== Pre-agent test (must fail) ==="
if ./run_test.sh; then
  echo "ERROR: run_test.sh passes before the agent runs — setup is broken"
  exit 1
fi
echo "OK: test fails as expected before agent fix"

mkdir -p "$tmp/.wg_state"
WG_DIR="$tmp/.wg_state" wg init --no-agency >/dev/null 2>&1 || true

echo
echo "=== Invoking wg nex --eval-mode ==="
stdout_file=$(mktemp)
stderr_file=$(mktemp)
trap "rm -f $stdout_file $stderr_file; rm -rf $tmp" EXIT

set +e
WG_DIR="$tmp/.wg_state" timeout 180 wg nex --eval-mode --max-turns 10 \
  'Edit greet.sh so it prints "hello moon" instead of "hello world". Use the edit_file or write_file tool. Then end your turn.' \
  >"$stdout_file" 2>"$stderr_file"
agent_rc=$?
set -e

echo "agent exit code: $agent_rc"
echo
echo "=== Agent stdout (should be a single JSON line) ==="
cat "$stdout_file"
echo
echo "=== Agent stderr (should be empty under eval-mode) ==="
if [ -s "$stderr_file" ]; then
  echo "WARN: stderr is non-empty — eval-mode should suppress it"
  cat "$stderr_file"
else
  echo "(empty — good)"
fi
echo

echo "=== Post-agent test (must pass if agent fixed it) ==="
if ./run_test.sh; then
  echo "PASS: agent produced a working fix"
else
  echo "FAIL: test still fails after agent run"
  echo "--- git diff vs initial ---"
  git diff HEAD
  echo "--- greet.sh ---"
  cat greet.sh
  exit 1
fi

echo
echo "=== JSON summary parse ==="
if ! command -v jq >/dev/null 2>&1; then
  echo "SKIP: jq not installed, not parsing JSON"
else
  status=$(jq -r '.status' < "$stdout_file")
  turns=$(jq -r '.turns' < "$stdout_file")
  exit_reason=$(jq -r '.exit_reason' < "$stdout_file")
  echo "status=$status turns=$turns exit_reason=$exit_reason"
  if [ "$status" != "ok" ]; then
    echo "FAIL: JSON status != ok (got: $status)"
    exit 1
  fi
fi

echo
echo "=== SUCCESS ==="
echo "wg nex --eval-mode works end-to-end for a SWE-bench-shaped task."
