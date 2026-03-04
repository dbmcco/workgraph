# Design: Model Cost Tracking in Agency Assignment

## Status
Design complete. Ready for implementation.

---

## Problem

The agency system assigns agents to tasks based on skills (role components), performance scores (avg_score from evaluations), and exploration (UCB1 with novelty bonus). It does **not** factor in model cost. When a budget model achieves comparable quality to a frontier model on a given task type, the system has no mechanism to prefer the cheaper option.

Today's state:
- **TokenUsage** exists on tasks (`graph.rs:276`) with `cost_usd`, `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`
- **ModelRegistry** exists (`models.rs`) with `cost_per_1m_input`, `cost_per_1m_output`, tier, capabilities per model
- **Agent** has an `executor` field but no `model` field — model is set at the coordinator level, not per-agent
- **Evaluation** has an optional `model` field (populated from spawn log parsing in `evaluate.rs`)
- **PerformanceRecord** tracks `avg_score` but not avg_cost
- **find_cached_agent** (`run_mode.rs:278`) selects purely on `avg_score >= threshold`

The gap: cost data flows through the system but is never used in assignment decisions.

---

## Design

### 1. Cost Data Flow (already mostly exists)

```
Executor spawns agent with --model X
  → Agent runs, executor reports token counts
  → Coordinator parses output.log → TokenUsage (cost_usd, tokens)
  → wg done stores TokenUsage on task
  → wg evaluate reads task.token_usage + spawn log model
  → Evaluation already has model field
```

**What's missing:** The Evaluation doesn't carry cost, and the PerformanceRecord doesn't aggregate cost data.

### 2. Extend Evaluation with Cost Data

Add cost fields to `Evaluation` (`agency/types.rs`):

```rust
/// An evaluation of agent performance on a specific task.
pub struct Evaluation {
    // ... existing fields ...

    /// Cost in USD for this task execution (from TokenUsage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,

    /// Token counts for this execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<EvalTokenUsage>,
}

/// Lightweight token usage snapshot stored in evaluations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}
```

The `evaluate.rs` command already has access to `task.token_usage` — it just needs to copy `cost_usd` and token counts into the Evaluation.

### 3. Extend PerformanceRecord with Cost Aggregates

Add cost tracking to `PerformanceRecord` (`agency/types.rs`):

```rust
pub struct PerformanceRecord {
    pub task_count: u32,
    pub avg_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evaluations: Vec<EvaluationRef>,

    /// Average cost in USD per task execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_cost_usd: Option<f64>,

    /// Total cost in USD across all evaluations.
    #[serde(default, skip_serializing_if = "is_f64_zero")]
    pub total_cost_usd: f64,
}
```

Update `eval.rs::update_performance` to accumulate cost:

```rust
pub fn update_performance(record: &mut PerformanceRecord, eval_ref: EvaluationRef) {
    record.task_count = record.task_count.saturating_add(1);
    record.evaluations.push(eval_ref);
    record.avg_score = recalculate_avg_score(&record.evaluations);
    // Cost accumulation handled separately via update_performance_cost
}

pub fn update_performance_cost(record: &mut PerformanceRecord, cost_usd: f64) {
    record.total_cost_usd += cost_usd;
    if record.task_count > 0 {
        record.avg_cost_usd = Some(record.total_cost_usd / record.task_count as f64);
    }
}
```

### 4. Cost-Adjusted Scoring for Assignment

The key insight: **a cheaper model that scores well enough is better than an expensive model that scores marginally higher.** This is a value-for-money calculation.

#### 4a. Cost-Efficiency Score

Introduce a cost-efficiency metric that the assignment system can use:

```
cost_efficiency = score / (1 + cost_weight * normalized_cost)
```

Where:
- `score` is the agent's `avg_score` (0–1)
- `normalized_cost` = `agent_avg_cost / max_avg_cost_across_agents` (0–1)
- `cost_weight` is a configurable parameter (default: 0.3)

When `cost_weight = 0`, the system behaves exactly as today (pure quality). When `cost_weight = 1`, cost and quality are weighted equally.

#### 4b. Modify `find_cached_agent`

Current (`run_mode.rs:278`):
```rust
pub fn find_cached_agent(agency_dir: &Path, threshold: f64) -> Option<(Agent, f64)> {
    // ... loads agents, filters by avg_score >= threshold, returns max score
}
```

Proposed:
```rust
pub fn find_cached_agent(
    agency_dir: &Path,
    threshold: f64,
    cost_weight: f64,
) -> Option<(Agent, f64)> {
    let agents = load_all_agents_or_warn(&agents_dir);
    let eligible: Vec<_> = agents
        .into_iter()
        .filter_map(|a| {
            let score = a.performance.avg_score?;
            if score >= threshold && a.staleness_flags.is_empty() {
                Some((a, score))
            } else {
                None
            }
        })
        .collect();

    if eligible.is_empty() || cost_weight <= 0.0 {
        // No cost weighting — pure quality (backward compatible)
        return eligible.into_iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
    }

    // Compute cost-efficiency scores
    let max_cost = eligible.iter()
        .filter_map(|(a, _)| a.performance.avg_cost_usd)
        .fold(0.0_f64, f64::max)
        .max(0.001); // avoid div-by-zero

    eligible.into_iter()
        .max_by(|(a, score_a), (b, score_b)| {
            let eff_a = cost_efficiency(*score_a, a.performance.avg_cost_usd, max_cost, cost_weight);
            let eff_b = cost_efficiency(*score_b, b.performance.avg_cost_usd, max_cost, cost_weight);
            eff_a.partial_cmp(&eff_b).unwrap_or(Ordering::Equal)
        })
}

fn cost_efficiency(score: f64, avg_cost: Option<f64>, max_cost: f64, weight: f64) -> f64 {
    let normalized_cost = avg_cost.unwrap_or(max_cost) / max_cost;
    score / (1.0 + weight * normalized_cost)
}
```

#### 4c. Extend UCB1 with Cost Term

For learning mode, the UCB1 formula gets a cost penalty:

```
ucb1_cost = (base_score + exploration_bonus) * novelty_factor - cost_penalty
```

Where `cost_penalty = cost_weight * normalized_avg_cost`. This gently steers exploration toward cheaper primitives when all else is equal.

### 5. Budget Constraints

Add budget fields to `AgencyConfig`:

```rust
pub struct AgencyConfig {
    // ... existing fields ...

    /// Weight of cost in assignment scoring (0.0 = ignore cost, 1.0 = equal to quality).
    /// Default: 0.0 (backward compatible — cost tracking enabled, but no scoring impact)
    #[serde(default)]
    pub cost_weight: f64,

    /// Maximum USD budget per task. Agents with avg_cost above this are
    /// excluded from assignment. None = no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_per_task: Option<f64>,

    /// Maximum USD budget for the entire project (across all tasks).
    /// When reached, only budget-tier models are eligible.
    /// None = no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_budget_usd: Option<f64>,
}
```

Budget enforcement in assignment:
1. **Per-task cap**: Filter out agents whose `avg_cost_usd > max_cost_per_task`
2. **Project cap**: Sum `cost_usd` across all completed tasks. If approaching budget, restrict to budget-tier models via `ModelRegistry::tier`
3. **Soft warning**: When 80% of project budget is consumed, log a warning

### 6. Model-Aware Agent Assignment

Currently agents have an `executor` field but no `model` field. The model is set globally via `wg config --model`. To enable cost-aware assignment, agents need model affinity:

**Option A (recommended): Keep model at coordinator level, track cost per agent empirically.**
Agents don't specify a model — the coordinator assigns models. Cost data comes from actual execution history (TokenUsage → Evaluation → PerformanceRecord). This is simpler and already works with the existing architecture.

**Option B: Add `preferred_model` to Agent.**
This couples agents to models, which conflicts with the composition system (an agent = role + tradeoff, not role + tradeoff + model). It also means evolving agents would need to consider model variants.

**Recommendation: Option A.** The agency system's strength is separating concerns (what to do vs. how to prioritize). Model selection is an executor concern. Cost tracking connects the two via empirical data — no structural coupling needed.

### 7. Integration Points

#### In `commands/evaluate.rs`
The evaluate command already parses `task.token_usage` and spawn model. Add:
```rust
// When constructing Evaluation:
eval.cost_usd = task.token_usage.as_ref().map(|u| u.cost_usd);
eval.token_usage = task.token_usage.as_ref().map(|u| EvalTokenUsage {
    input_tokens: u.input_tokens,
    output_tokens: u.output_tokens,
});
```

#### In `agency/eval.rs::record_evaluation`
After updating each entity's PerformanceRecord, also call `update_performance_cost` if cost data is present.

#### In `commands/agency_stats.rs`
Add cost columns to stats output: avg_cost per agent, total cost per role, cost efficiency ranking.

#### In `wg agents` display
Show cost column alongside score, turns, tokens.

### 8. Migration

All new fields use `#[serde(default)]` so existing data deserializes without changes. Cost data is `None`/`0.0` for historical evaluations. The system bootstraps cost tracking organically as new evaluations arrive.

`cost_weight` defaults to `0.0`, so assignment behavior is unchanged until explicitly configured.

---

## Implementation Order

1. **Add `cost_usd` and `token_usage` to Evaluation** — data capture (types.rs, evaluate.rs)
2. **Add `avg_cost_usd` and `total_cost_usd` to PerformanceRecord** — aggregation (types.rs, eval.rs)
3. **Add `cost_weight`, `max_cost_per_task`, `project_budget_usd` to AgencyConfig** — configuration (config.rs)
4. **Modify `find_cached_agent`** to use cost-efficiency scoring (run_mode.rs)
5. **Extend UCB1** with cost penalty term (run_mode.rs)
6. **Add budget enforcement** in coordinator assignment path (service/)
7. **Surface cost data** in `wg agents`, `wg agency stats` (display)

Steps 1-3 are pure data plumbing with no behavioral change. Step 4 activates cost-aware assignment. Steps 5-7 are extensions.

---

## Open Questions

1. **Cache read tokens pricing**: Cache reads are cheaper than fresh input tokens for Anthropic. The `ModelRegistry` currently has `cost_per_1m_input` and `cost_per_1m_output` but no cache-read price. For now, use the `cost_usd` field from `TokenUsage` (which already includes the executor's own cost calculation) rather than recomputing from token counts.

2. **Cost decay**: Should older cost data be weighted less? Model prices change. For now, use simple average — the ModelRegistry can be updated with current prices, and stale evaluations will be diluted by new ones.

3. **Multi-model agents**: If the same agent (role + tradeoff) runs on different models across tasks, `avg_cost_usd` blends those costs. This is intentional — it reflects the empirical cost of deploying that composition. If you want to distinguish, evolve a new agent variant.
