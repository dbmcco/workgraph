"""
Terminal Bench Condition B Harness: Agent + Workgraph (Treatment Group)

This adapter implements Harbor's agent protocol for Terminal Bench evaluation.
It provides a full workgraph-integrated agent with graph awareness, journal/resume,
and task decomposition capabilities.

Condition B characteristics:
- Native executor with full wg tool access
- Tools: everything in Condition A + wg_show, wg_list, wg_add, wg_done, wg_fail,
  wg_log, wg_artifact, wg_msg_send, wg_msg_read
- Journal/resume enabled (survives context exhaustion)
- System prompt: scope-based assembly (task context + graph awareness + wg CLI)
- Agent can: decompose into subtasks, log progress, create verification gates

This is the TREATMENT GROUP - the thesis of the memory paper.
"""

import json
import os
import subprocess
import tempfile
import uuid
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional

import yaml


# ─────────────────────────────────────────────────────────────────────────────
# Harbor Agent Protocol Interface
# ─────────────────────────────────────────────────────────────────────────────

class Agent:
    """
    Terminal Bench agent adapter implementing Harbor's agent protocol.
    
    This is the Condition B (agent + workgraph) harness that:
    - Uses the native Rust executor via `wg native-exec`
    - Runs with full wg tools (wg_show, wg_list, wg_add, etc.)
    - Journal/resume enabled for crash recovery
    - Scope-based system prompt assembly
    - wg binary injection into Docker containers
    """
    
    def __init__(
        self,
        model: str = "minimax/minimax-m2.7",
        max_turns: int = 100,
        timeout_seconds: int = 1800,
        openrouter_api_key: Optional[str] = None,
        wg_binary_path: Optional[str] = None,
        condition: str = "B",
    ):
        """
        Initialize the Condition A agent adapter.
        
        Args:
            model: Model to use via OpenRouter (e.g., "minimax/minimax-m2.7")
            max_turns: Maximum agent turns before stopping
            timeout_seconds: Task timeout in seconds
            openrouter_api_key: OpenRouter API key (falls back to env)
            wg_binary_path: Path to wg binary (falls back to system PATH)
            condition: "A" for bare agent, "B" for agent + workgraph
        """
        self.model = model
        self.max_turns = max_turns
        self.timeout_seconds = timeout_seconds
        self.openrouter_api_key = openrouter_api_key or os.environ.get("OPENROUTER_API_KEY")
        self.wg_binary_path = wg_binary_path or self._find_wg_binary()
        self.condition = condition  # "A" or "B"
        
    def _find_wg_binary(self) -> str:
        """Find the wg binary."""
        # Check common locations
        candidates = [
            "/home/erik/workgraph/target/release/wg",
            "/home/erik/workgraph/target/debug/wg",
            "wg",  # System PATH
        ]
        for path in candidates:
            if os.path.exists(path):
                return path
        # Fall back to system PATH
        return "wg"
    
    def run(
        self,
        task_instruction: str,
        working_dir: Optional[str] = None,
        container_id: Optional[str] = None,
    ) -> Dict[str, Any]:
        """
        Run a Terminal Bench task using the native executor.
        
        Args:
            task_instruction: The task description from Terminal Bench
            working_dir: Working directory for the task (maps to Docker volume mount)
            container_id: Docker container ID if running inside a container
            
        Returns:
            Dict with keys: success, output, error, turns, tokens_used
        """
        task_id = f"tb-condition-{self.condition.lower()}-{uuid.uuid4().hex[:8]}"
        workgraph_dir = tempfile.mkdtemp(prefix="wg-tb-")
        
        try:
            # For Condition B: inject wg binary and initialize workgraph
            if self.condition == "B":
                self._inject_wg_into_container(container_id)
                self._initialize_workgraph(workgraph_dir, task_id, task_instruction, container_id)
            
            # Build the prompt file with Condition-appropriate system prompt
            prompt_file = os.path.join(workgraph_dir, "prompt.txt")
            system_prompt = self._build_system_prompt(task_instruction)
            with open(prompt_file, "w") as f:
                f.write(system_prompt)
            
            # Build the native-exec command
            cmd = self._build_native_exec_command(
                task_id=task_id,
                prompt_file=prompt_file,
                workgraph_dir=workgraph_dir,
                working_dir=working_dir,
            )
            
            # Execute with timeout
            result = self._execute_with_timeout(cmd, container_id)
            
            # Parse output and extract results
            return self._parse_results(
                task_id=task_id,
                result=result,
                workgraph_dir=workgraph_dir,
            )
            
        finally:
            # Cleanup workgraph directory (keep for debugging if needed)
            import shutil
            shutil.rmtree(workgraph_dir, ignore_errors=True)
    
    def _inject_wg_into_container(self, container_id: Optional[str]) -> None:
        """
        Inject the wg binary into the Docker container for Condition B.
        
        Uses docker cp to copy the binary into the container.
        """
        if not container_id:
            return
            
        # Find wg binary on host
        wg_path = self._find_wg_binary()
        if not os.path.exists(wg_path):
            raise FileNotFoundError(f"wg binary not found at: {wg_path}")
        
        # Copy wg binary into container
        subprocess.run(
            ["docker", "cp", wg_path, f"{container_id}:/usr/local/bin/wg"],
            check=True,
            capture_output=True,
        )
        
        # Make it executable
        subprocess.run(
            ["docker", "exec", container_id, "chmod", "+x", "/usr/local/bin/wg"],
            check=True,
            capture_output=True,
        )
    
    def _initialize_workgraph(
        self,
        workgraph_dir: str,
        task_id: str,
        task_instruction: str,
        container_id: Optional[str],
    ) -> None:
        """
        Initialize workgraph in the container for Condition B.
        
        Creates .workgraph/ directory and root task from Terminal Bench instruction.
        """
        if not container_id:
            return
        
        # Create .workgraph directory in container
        subprocess.run(
            ["docker", "exec", container_id, "wg", "init"],
            check=True,
            capture_output=True,
            env={**os.environ, "WG_DIR": "/root/.workgraph"},
        )
        
        # Create the root task from the Terminal Bench instruction
        # Format the task instruction for wg add
        task_title = task_instruction[:100] + ("..." if len(task_instruction) > 100 else "")
        subprocess.run(
            ["docker", "exec", container_id, "wg", "add", task_title],
            check=True,
            capture_output=True,
            env={**os.environ, "WG_DIR": "/root/.workgraph"},
        )
    
    def _build_system_prompt(self, task_instruction: str) -> str:
        """
        Build the system prompt based on condition.
        
        Condition A: Minimal (tool descriptions + task instruction)
        Condition B: Scope-based assembly (task context + graph awareness + wg CLI instructions)
        """
        if self.condition == "B":
            return self._build_condition_b_prompt(task_instruction)
        else:
            return self._build_condition_a_prompt(task_instruction)
    
    def _build_condition_a_prompt(self, task_instruction: str) -> str:
        """
        Build Condition A system prompt: minimal, no graph awareness.
        
        This is intentionally bare - just tool descriptions and the task.
        """
        tools_description = """You have access to the following tools for completing the task:

## Tool: bash
Execute a shell command and return its output (stdout + stderr).
- Input: {"command": "shell command to execute", "timeout": optional_timeout_ms}
- Returns: Command output or error message

## Tool: read_file
Read the contents of a file.
- Input: {"path": "path to file", "offset": optional_line_number, "limit": optional_max_lines}
- Returns: File contents or error

## Tool: write_file
Write content to a file (creates or overwrites).
- Input: {"path": "path to file", "content": "content to write"}
- Returns: Success or error

## Tool: edit_file
Make a targeted edit to an existing file.
- Input: {"path": "path to file", "old_string": "exact text to find", "new_string": "replacement text"}
- Returns: Success or error

## Tool: glob
Find files matching a glob pattern.
- Input: {"path": "base directory", "pattern": "glob pattern (e.g., **/*.py)"}
- Returns: List of matching file paths

## Tool: grep
Search file contents using regex.
- Input: {"path": "file or directory to search", "pattern": "regex pattern"}
- Returns: Matching lines with file paths and line numbers

## Guidelines
- Always prefer precise edits over full file rewrites when possible
- Use glob and grep to explore the codebase before making changes
- Commands are executed in the task working directory
- Keep output concise - prefer summary over raw dump for large outputs
"""
        
        condition_a_prefix = """You are a coding agent completing a Terminal Bench task.
You have access to bash and file tools as described below.
Focus on completing the task efficiently and correctly.
Do not ask for clarification - proceed with your best judgment.
"""
        
        return f"{condition_a_prefix}\n\n{tools_description}\n\n## Task\n\n{task_instruction}"
    
    def _build_condition_b_prompt(self, task_instruction: str) -> str:
        """
        Build Condition B system prompt: scope-based assembly with full workgraph integration.
        
        This includes:
        - Task context + graph awareness
        - wg CLI instructions (REQUIRED_WORKFLOW_SECTION)
        - Graph patterns vocabulary (GRAPH_PATTERNS_SECTION)
        - Task decomposition guidance (AUTOPOIETIC_GUIDANCE)
        - Journal/resume awareness
        """
        # Core task assignment
        prompt_parts = [
            "# Task Assignment\n",
            "You are an AI agent working on a task in a workgraph project.\n",
            f"## Your Task\n- **Title:** Terminal Bench Task\n- **Description:** {task_instruction}",
            "",
        ]
        
        # Tool descriptions for Condition B (includes all wg tools)
        tools_description = """You have access to the following tools for completing the task:

## File Tools

## Tool: bash
Execute a shell command and return its output (stdout + stderr).
- Input: {"command": "shell command to execute", "timeout": optional_timeout_ms}
- Returns: Command output or error message

## Tool: read_file
Read the contents of a file.
- Input: {"path": "path to file", "offset": optional_line_number, "limit": optional_max_lines}
- Returns: File contents or error

## Tool: write_file
Write content to a file (creates or overwrites).
- Input: {"path": "path to file", "content": "content to write"}
- Returns: Success or error

## Tool: edit_file
Make a targeted edit to an existing file.
- Input: {"path": "path to file", "old_string": "exact text to find", "new_string": "replacement text"}
- Returns: Success or error

## Tool: glob
Find files matching a glob pattern.
- Input: {"path": "base directory", "pattern": "glob pattern (e.g., **/*.py)"}
- Returns: List of matching file paths

## Tool: grep
Search file contents using regex.
- Input: {"path": "file or directory to search", "pattern": "regex pattern"}
- Returns: Matching lines with file paths and line numbers

## Workgraph Tools

## Tool: wg_show
Show details of a workgraph task.
- Input: {"task_id": "the task ID to show"}
- Returns: Task details including status, artifacts, logs, and description

## Tool: wg_list
List tasks in the workgraph, optionally filtered by status.
- Input: {"status": "open|done|failed|in-progress|blocked"}
- Returns: List of tasks with their IDs and titles

## Tool: wg_add
Create a new task in the workgraph.
- Input: {"title": "task title", "after": "comma-separated task IDs (dependencies)", "description": "detailed description", "tags": "comma-separated tags"}
- Returns: Confirmation of task creation

## Tool: wg_done
Mark a task as done.
- Input: {"task_id": "the task ID to mark as done", "converged": "true if this is a cycle convergence"}
- Returns: Confirmation

## Tool: wg_fail
Mark a task as failed.
- Input: {"task_id": "the task ID", "reason": "reason for failure"}
- Returns: Confirmation

## Tool: wg_log
Append a log entry to a task.
- Input: {"task_id": "the task ID", "message": "log message"}
- Returns: Confirmation

## Tool: wg_artifact
Record an artifact (file path) for a task.
- Input: {"task_id": "the task ID", "path": "path to the artifact file"}
- Returns: Confirmation

## Tool: wg_msg_send
Send a message to a task's message queue.
- Input: {"task_id": "the task ID", "message": "message content"}
- Returns: Confirmation

## Tool: wg_msg_read
Read messages for a task.
- Input: {"task_id": "the task ID", "agent_id": "optional agent ID to filter"}
- Returns: List of messages

## Guidelines
- Always prefer precise edits over full file rewrites when possible
- Use glob and grep to explore the codebase before making changes
- Use wg tools to track progress, create subtasks, and coordinate with other agents
- Keep output concise - prefer summary over raw dump for large outputs
- CRITICAL: Use `wg log` to track progress - this enables crash recovery via journal/resume
"""
        
        prompt_parts.append(tools_description)
        
        # Required Workflow section
        workflow_section = """## Required Workflow

You MUST use these commands to track your work:

0. **Check for messages and reply** (BEFORE any other work):
   ```bash
   wg msg read <task_id> --agent $WG_AGENT_ID
   ```
   For EACH message, reply with what you'll do about it:
   ```bash
   wg msg send <task_id> "Acknowledged — will fix the prefix on line 42"
   ```
   Unreplied messages = incomplete task. This is not optional.

1. **Log progress** as you work (helps recovery if interrupted):
   ```bash
   wg log <task_id> "Starting implementation..."
   wg log <task_id> "Completed X, now working on Y"
   ```
   If you received messages in step 0, reply to them too (`wg msg send`).

2. **Record artifacts** if you create/modify files:
   ```bash
   wg artifact <task_id> path/to/file
   ```

3. **Validate your work** before marking done:
   - **Check task-specific criteria first:** Run `wg show <task_id>` and look for a **Verification Required** section or a **## Validation** section in the description. Those criteria are your primary acceptance test — address every item.
   - **Code tasks:** Run `cargo build` and `cargo test` (or the project's equivalent). Fix any failures.
   - **Research/docs tasks:** Re-read the task description and verify your output addresses every requirement. Check that referenced files and links exist.
   - **All tasks:** Log your validation results:
     ```bash
     wg log <task_id> "Validated: task-specific criteria met"
     wg log <task_id> "Validated: cargo build + cargo test pass"
     ```

4. **Commit and push** if you modified files:
   - Run `cargo build` and `cargo test` BEFORE committing — never commit broken code
   - Stage ONLY your files (never `git add -A`) and commit with a descriptive message:
     ```bash
     git add <your-files> && git commit -m "feat: <description>"
     git push
     ```
   - Log the commit hash:
     ```bash
     wg log <task_id> "Committed: $(git rev-parse --short HEAD) — pushed to remote"
     ```

5. **Check messages AGAIN and reply** (BEFORE marking done — this is a completion gate):
   ```bash
   wg msg read <task_id> --agent $WG_AGENT_ID
   ```
   Reply to ALL new messages before proceeding:
   ```bash
   wg msg send <task_id> "Done — applied the requested changes in commit abc123"
   ```
   If you skip replies, the task is incomplete. Do NOT mark done with unreplied messages.

6. **Complete the task** when done:
   ```bash
   wg done <task_id>
   wg done <task_id> --converged  # Use this if task has loop edges and work is complete
   ```

7. **Mark as failed** if you cannot complete:
   ```bash
   wg fail <task_id> --reason "Specific reason why"
   ```

## Important
- Run `wg log` commands BEFORE doing work to track progress
- Validate BEFORE running `wg done`
- Commit and push your changes BEFORE running `wg done`
- Run `wg done` BEFORE you finish responding
- If the task description is unclear, do your best interpretation
"""
        
        prompt_parts.append(workflow_section)
        
        # Graph Patterns section
        graph_patterns_section = """## Graph Patterns

**Vocabulary:** pipeline (A→B→C), diamond (A→[B,C,D]→E), scatter-gather (heterogeneous reviewers of same artifact), loop (A→B→C→A with `--max-iterations`).

**Golden rule: same files = sequential edges.** NEVER parallelize tasks that modify the same files — one will overwrite the other. When unsure, default to pipeline.

**Cycles (back-edges):** Workgraph is a directed graph, NOT a DAG. For repeating workflows (cleanup→commit→verify, write→review, etc.), create ONE cycle with `--max-iterations` instead of duplicating tasks for each pass. Use `wg done --converged` to stop the cycle when no more changes are needed. If you are inside a cycle, check `wg show` for your `loop_iteration` and evaluate whether the work has converged before deciding to iterate or stop.

**When creating subtasks:**
- Always include an integrator task at join points: `wg add "Integrate" --after worker-a,worker-b`
- List each worker's file scope in the task description
- Run `wg quickstart` for full command reference

**After code changes:** Run `cargo install --path .` to update the global binary.
"""
        
        prompt_parts.append(graph_patterns_section)
        
        # Task Decomposition section
        decomposition_section = """## Task Decomposition

You are encouraged to create new tasks as you discover work. The coordinator will dispatch them automatically.

### When to decompose
- Your task has 3+ independent parts that could run in parallel
- You discover a bug, missing doc, or needed refactor outside your scope
- A prerequisite doesn't exist yet and needs to be created first
- Your task is too large for a single agent session

### How to decompose
- **Fan out parallel work**: `wg add 'Part A' --after <task_id>` and `wg add 'Part B' --after <task_id>`
- **Create a synthesis task**: After fan-out, add an integrator: `wg add 'Integrate results' --after part-a,part-b`
- **Pipeline decomposition**: `wg add 'Step 1' --after <task_id> && wg add 'Step 2' --after step-1`
- **Bug/issue found**: `wg add 'Fix: ...' --after <task_id> -d 'Found while working on <task_id>'`

### Include validation criteria in subtasks
Every code subtask description MUST include a `## Validation` section with concrete acceptance criteria.

### Guardrails
- You can create up to **20** subtasks per session
- Task chains have a maximum depth of **10** levels
- Always include an integrator at join points — don't leave parallel work unmerged

### When NOT to decompose
- The task is small and well-scoped (just do it)
- Decomposition overhead exceeds the work itself
- The subtasks would all modify the same files (serialize instead)
"""
        
        prompt_parts.append(decomposition_section)
        
        # Message polling section
        message_section = """## Messages

Check for new messages periodically during long-running tasks:
```bash
wg msg read <task_id> --agent $WG_AGENT_ID
```
Messages may contain updated requirements, context from other agents, or instructions from the user.

If there are messages, reply to each one:
```bash
wg msg send <task_id> "Acknowledged — adjusting approach per your feedback"
```
"""
        
        prompt_parts.append(message_section)
        
        # Journal/Resume awareness
        journal_section = """## Journal/Resume

This agent supports journal-based crash recovery. Your progress is automatically persisted.

- **Logging progress**: Use `wg log <task_id> "message"` to record what you completed. If the agent crashes and resumes, it will read your log entries and continue from where you left off.
- **Context exhaustion**: If you approach context limits, log your progress before the session ends. The resumed agent will see what you accomplished.
- **Subtask creation**: Creating subtasks with `wg add` persists across sessions - they are stored in the workgraph, not in context.

This is the key advantage over Condition A: you can work on complex tasks across multiple sessions without losing progress.
"""
        
        prompt_parts.append(journal_section)
        
        # Begin work
        prompt_parts.append("\nBegin working on the task now.")
        
        return "\n\n".join(prompt_parts)
    
    def _build_native_exec_command(
        self,
        task_id: str,
        prompt_file: str,
        workgraph_dir: str,
        working_dir: Optional[str],
    ) -> List[str]:
        """Build the wg native-exec command based on condition."""
        # Create bundle based on condition
        bundles_dir = os.path.join(workgraph_dir, "bundles")
        os.makedirs(bundles_dir, exist_ok=True)
        
        if self.condition == "B":
            # Condition B bundle: all tools including wg tools, journal/resume enabled
            condition_bundle = """name = "condition-b"
description = "Terminal Bench Condition B: Full workgraph integration. All tools + wg tools, journal/resume enabled."
tools = ["*"]
context_scope = "full"
"""
            bundle_name = "condition-b"
            exec_mode = "full"  # Full tool access including wg tools
            resume_enabled = True
        else:
            # Condition A bundle: bash + file tools only, NO wg tools
            condition_bundle = """name = "condition-a"
description = "Terminal Bench Condition A: Bare agent control group. No wg tools, no graph awareness."
tools = ["bash", "read_file", "write_file", "edit_file", "glob", "grep"]
context_scope = "clean"
"""
            bundle_name = "condition-a"
            exec_mode = "condition-a"  # Custom bundle: bash + file tools only
            resume_enabled = False
        
        bundle_path = os.path.join(bundles_dir, f"{bundle_name}.toml")
        with open(bundle_path, "w") as f:
            f.write(condition_bundle)
        
        cmd = [
            self.wg_binary_path,
            "native-exec",
            "--dir", workgraph_dir,
            "--prompt-file", prompt_file,
            "--task-id", task_id,
            "--model", self.model,
            "--exec-mode", exec_mode,
            "--max-turns", str(self.max_turns),
        ]
        
        # Only enable resume for Condition B
        if not resume_enabled:
            cmd.append("--no-resume")
        
        # Set working directory
        if working_dir:
            cmd.extend(["--working-dir", working_dir])
        
        # Set OpenRouter API key if provided
        if self.openrouter_api_key:
            cmd.extend(["--api-key", self.openrouter_api_key])
        
        return cmd
    
    def _execute_with_timeout(
        self,
        cmd: List[str],
        container_id: Optional[str],
    ) -> subprocess.CompletedProcess:
        """Execute command with timeout."""
        env = os.environ.copy()
        if self.openrouter_api_key:
            env["OPENROUTER_API_KEY"] = self.openrouter_api_key
        
        # If running inside a container, execute via docker exec
        if container_id:
            docker_cmd = [
                "docker", "exec",
                "-w", "/workspace",
                container_id,
            ] + cmd
            process = subprocess.run(
                docker_cmd,
                capture_output=True,
                text=True,
                timeout=self.timeout_seconds,
                env=env,
            )
        else:
            process = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=self.timeout_seconds,
                env=env,
            )
        
        return process
    
    def _parse_results(
        self,
        task_id: str,
        result: subprocess.CompletedProcess,
        workgraph_dir: str,
    ) -> Dict[str, Any]:
        """Parse execution results into standardized format."""
        # Look for output log
        output_log = os.path.join(workgraph_dir, "native-exec.ndjson")
        agent_log = os.path.join(workgraph_dir, "agent.ndjson")
        
        turns = 0
        tokens_used = {"input": 0, "output": 0}
        final_text = ""
        error_output = []
        
        # Parse NDJSON log if exists
        for log_path in [output_log, agent_log]:
            if os.path.exists(log_path):
                try:
                    with open(log_path, "r") as f:
                        for line in f:
                            try:
                                event = json.loads(line)
                                if event.get("type") == "Result":
                                    turns = event.get("turns", 0)
                                    usage = event.get("total_usage", {})
                                    tokens_used = {
                                        "input": usage.get("input_tokens", 0),
                                        "output": usage.get("output_tokens", 0),
                                    }
                                    final_text = event.get("final_text", "")
                            except json.JSONDecodeError:
                                continue
                except Exception:
                    pass
        
        # Determine success based on exit code and output
        success = result.returncode == 0
        
        # Check for error indicators in output
        if not success:
            error_output = [result.stderr] if result.stderr else []
        
        return {
            "success": success,
            "task_id": task_id,
            "output": final_text or result.stdout,
            "error": "\n".join(error_output) if error_output else None,
            "turns": turns,
            "tokens_used": tokens_used,
            "exit_code": result.returncode,
            "condition": self.condition,  # "A" or "B" based on which harness was used
        }


# ─────────────────────────────────────────────────────────────────────────────
# Harbor Agent Protocol - WorkgraphAgent Class
# ─────────────────────────────────────────────────────────────────────────────

class WorkgraphAgent:
    """
    Harbor agent interface implementation for Terminal Bench.
    
    This class is instantiated by Harbor for each task evaluation.
    It wraps the Agent to provide Harbor's expected interface.
    
    Usage:
        harbor run --agent-import-path wg.adapter:WorkgraphAgent -m minimax/minimax-m2.7 ...
        
    For Condition B (full workgraph integration):
        harbor run --agent-import-path wg.adapter:WorkgraphAgent -m minimax/minimax-m2.7 --condition B ...
    """
    
    def __init__(
        self,
        model: str = "minimax/minimax-m2.7",
        max_turns: int = 100,
        timeout_seconds: int = 1800,
        condition: str = "B",
    ):
        """
        Initialize the WorkgraphAgent for Harbor.
        
        Args:
            model: Model identifier for OpenRouter (e.g., "minimax/minimax-m2.7")
            max_turns: Maximum turns per task
            timeout_seconds: Task timeout
            condition: "A" for bare agent, "B" for agent + workgraph (default: "B")
        """
        self.agent = Agent(
            model=model,
            max_turns=max_turns,
            timeout_seconds=timeout_seconds,
            condition=condition,
        )
    
    def run(self, task_instruction: str, **kwargs) -> Dict[str, Any]:
        """
        Run a Terminal Bench task.
        
        Args:
            task_instruction: The task description from Terminal Bench
            **kwargs: Additional Harbor parameters (container_id, working_dir)
            
        Returns:
            Dict with: success, output, error, turns, tokens_used
        """
        container_id = kwargs.get("container_id")
        working_dir = kwargs.get("working_dir")
        return self.agent.run(
            task_instruction=task_instruction,
            working_dir=working_dir,
            container_id=container_id,
        )


# ─────────────────────────────────────────────────────────────────────────────
# CLI Entry Point (for Harbor integration)
# ─────────────────────────────────────────────────────────────────────────────

def main():
    """
    CLI entry point for the adapter.
    
    Can be used directly or via Harbor's --agent-import-path option:
        harbor run --agent-import-path wg.adapter:WorkgraphAgent ...
        
    For Condition A (bare agent):
        harbor run --agent-import-path wg.adapter:WorkgraphAgent --condition A ...
    
    For Condition B (agent + workgraph, default):
        harbor run --agent-import-path wg.adapter:WorkgraphAgent --condition B ...
    """
    import argparse
    
    parser = argparse.ArgumentParser(
        description="Terminal Bench Harness (Workgraph Agent)"
    )
    parser.add_argument(
        "--model",
        default="minimax/minimax-m2.7",
        help="Model to use via OpenRouter (default: minimax/minimax-m2.7)",
    )
    parser.add_argument(
        "--max-turns",
        type=int,
        default=100,
        help="Maximum agent turns (default: 100)",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=1800,
        help="Task timeout in seconds (default: 1800)",
    )
    parser.add_argument(
        "--condition",
        default="B",
        choices=["A", "B"],
        help="Condition: A=bare agent, B=agent + workgraph (default: B)",
    )
    
    args = parser.parse_args()
    
    agent = Agent(
        model=args.model,
        max_turns=args.max_turns,
        timeout_seconds=args.timeout,
        condition=args.condition,
    )
    
    print(f"Terminal Bench Harness initialized with model: {agent.model}")
    print(f"Condition: {agent.condition}")
    if agent.condition == "B":
        print(f"Tools: bash, read_file, write_file, edit_file, glob, grep + wg tools")
        print(f"Features: journal/resume enabled, scope-based prompt, task decomposition")
    else:
        print(f"Tools: bash, read_file, write_file, edit_file, glob, grep")
        print(f"Note: Bare agent control group - no wg tools enabled")


# ─────────────────────────────────────────────────────────────────────────────
# Alternative: Direct Python API
# ─────────────────────────────────────────────────────────────────────────────

@dataclass
class TaskResult:
    """Result from a single task execution."""
    success: bool
    task_id: str
    output: str
    error: Optional[str] = None
    turns: int = 0
    tokens_used: Dict[str, int] = field(default_factory=dict)
    exit_code: int = 0
    condition: str = "B"  # Default to Condition B (treatment group)


def run_task(
    task_instruction: str,
    model: str = "minimax/minimax-m2.7",
    max_turns: int = 100,
    timeout_seconds: int = 1800,
    working_dir: Optional[str] = None,
    condition: str = "B",
) -> TaskResult:
    """
    Run a single Terminal Bench task with the specified condition.
    
    Args:
        task_instruction: The task description from Terminal Bench
        model: Model to use via OpenRouter
        max_turns: Maximum agent turns
        timeout_seconds: Task timeout
        working_dir: Optional working directory
        condition: "A" for bare agent, "B" for agent + workgraph (default: "B")
        
    Returns:
        TaskResult with execution details
    """
    agent = Agent(
        model=model,
        max_turns=max_turns,
        timeout_seconds=timeout_seconds,
        condition=condition,
    )
    
    result = agent.run(
        task_instruction=task_instruction,
        working_dir=working_dir,
    )
    
    return TaskResult(**result)


if __name__ == "__main__":
    main()
