//! `SpawnPlan`: the single struct describing what runs for a task spawn.
//!
//! ## Precedence (executor)
//!
//! 1. `task.exec` set, or `task.exec_mode == "shell"`  →  `Shell`  (final)
//! 2. Per-task explicit override (currently `task.exec_mode` mapping to a
//!    known executor, or future `task.executor` field)                  →  final
//! 3. Agency-derived `agent_executor` (passed in by caller)              → hint
//! 4. Local/global `[dispatcher].executor` (a.k.a. `coordinator.executor`) → hint
//! 5. Default (`claude`)
//!
//! Provider-qualified model specs are authoritative. A stale
//! `[dispatcher].executor = "claude"` paired with `model = "zai:glm-5.1"` or
//! `model = "openrouter:…"` must not launch the Claude CLI; the provider
//! prefix selects the compatible handler.
//!
//! ## Precedence (endpoint)
//!
//! Endpoint is **executor-scoped**:
//!
//! - `executor=claude`  →  endpoint is always `None` (the claude CLI handles
//!   auth/url itself). Even if a global default endpoint exists, we do not
//!   pass `--endpoint`.
//! - `executor=shell`   →  endpoint is always `None`.
//! - `executor=codex`   →  endpoint is always `None` (codex CLI handles its own).
//! - external worker CLIs (`opencode`, `aider`, etc.) → endpoint is always
//!   `None` (their adapters handle provider/auth policy).
//! - `executor=native` / `nex` → endpoint is required; resolved via merged config
//!   (per-task → role → default).
//!
//! ## Provenance
//!
//! Every `SpawnPlan` carries a `SpawnProvenance` recording *which config
//! knob produced which value*. This is logged on every spawn so you can
//! always answer "why did this task spawn `native --endpoint openrouter`?"
//! by reading one line.

use crate::config::{Config, EndpointConfig, parse_model_spec, provider_to_executor};
use crate::graph::Task;
use crate::service::{ProviderHealth, extract_provider_id};
use anyhow::{Result, anyhow};
use std::collections::HashMap;

/// The executor kind that will run a spawned agent. This is the canonical
/// type; string forms (`"claude"`, `"native"`, …) are an external interop
/// concern — internally we should always pass an `ExecutorKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    /// Claude Code CLI session. Handles its own auth/url.
    Claude,
    /// In-process native executor (`wg native-exec …`). Speaks OpenAI-compat
    /// or Anthropic wire format; needs an explicit endpoint.
    Native,
    /// Shell executor: runs `task.exec` verbatim. No model, no endpoint.
    Shell,
    /// Codex CLI (`codex exec …`). Handles its own auth.
    Codex,
    /// OpenCode CLI (`opencode run …`). Handles its own provider auth and has
    /// a WG live-chat handler.
    OpenCode,
    /// Aider CLI worker. One-shot task executor only, not a live chat handler.
    Aider,
    /// Goose CLI worker. One-shot task executor only, not a live chat handler.
    Goose,
    /// Qwen Code CLI worker. One-shot task executor only, not a live chat handler.
    Qwen,
    /// Cline CLI worker. One-shot task executor only, not a live chat handler.
    Cline,
    /// Crush CLI worker. One-shot task executor only, not a live chat handler.
    Crush,
    /// Amplifier CLI worker. Experimental one-shot task executor only.
    Amplifier,
    /// Octomind CLI. Chat-capable external CLI (`octomind run`); currently
    /// integrated for the TUI live-chat PTY path only (no spawn-task handler).
    Octomind,
    /// Dexto CLI. Chat-capable external CLI (`dexto --agent`); currently
    /// integrated for the TUI live-chat PTY path only (no spawn-task handler).
    Dexto,
    /// Pi CLI (pi.dev). Chat-capable external CLI. It is prefix-addressed
    /// (`pi:...`) and therefore part of EXTERNAL_CLIS, but it is not
    /// worker-only; the handler/RPC contract lands separately.
    Pi,
}

impl ExecutorKind {
    /// External CLI adapters addressed by an *executor* name prefix
    /// (`opencode:…`, `aider:…`, …) rather than a model-provider prefix.
    /// These are intentionally NOT model providers — an executor is not a
    /// provider — so prefix-routing code (`parse_executor_model_route`,
    /// `handler_for_model`, profile activation, `wg add` model resolution)
    /// keys off this set, not `KNOWN_PROVIDERS`.
    ///
    /// Membership here says nothing about chat-capability; see
    /// [`WORKER_ONLY_EXTERNALS`](Self::WORKER_ONLY_EXTERNALS) for that.
    pub const EXTERNAL_CLIS: &'static [ExecutorKind] = &[
        ExecutorKind::OpenCode,
        ExecutorKind::Aider,
        ExecutorKind::Goose,
        ExecutorKind::Qwen,
        ExecutorKind::Cline,
        ExecutorKind::Crush,
        ExecutorKind::Amplifier,
        ExecutorKind::Octomind,
        ExecutorKind::Dexto,
        ExecutorKind::Pi,
    ];

    /// The subset of [`EXTERNAL_CLIS`](Self::EXTERNAL_CLIS) that can ONLY run
    /// as one-shot task-agent workers and do NOT implement WG's live
    /// chat/session protocol. `OpenCode` is deliberately absent: it ships a
    /// live chat handler (`wg opencode-handler --chat`), so it is an external
    /// CLI that is *also* chat-capable. `Octomind` / `Dexto` are likewise
    /// absent: they are chat-capable external CLIs wired into the TUI
    /// live-chat PTY path (see `prototype-octomind-dexto-chat`). `Pi` is also
    /// absent: it is a chat-capable external CLI whose handler is wired in a
    /// later phase.
    pub const WORKER_ONLY_EXTERNALS: &'static [ExecutorKind] = &[
        ExecutorKind::Aider,
        ExecutorKind::Goose,
        ExecutorKind::Qwen,
        ExecutorKind::Cline,
        ExecutorKind::Crush,
        ExecutorKind::Amplifier,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ExecutorKind::Claude => "claude",
            ExecutorKind::Native => "native",
            ExecutorKind::Shell => "shell",
            ExecutorKind::Codex => "codex",
            ExecutorKind::OpenCode => "opencode",
            ExecutorKind::Aider => "aider",
            ExecutorKind::Goose => "goose",
            ExecutorKind::Qwen => "qwen",
            ExecutorKind::Cline => "cline",
            ExecutorKind::Crush => "crush",
            ExecutorKind::Amplifier => "amplifier",
            ExecutorKind::Octomind => "octomind",
            ExecutorKind::Dexto => "dexto",
            ExecutorKind::Pi => "pi",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" => Some(ExecutorKind::Claude),
            "native" | "nex" => Some(ExecutorKind::Native),
            "shell" => Some(ExecutorKind::Shell),
            "codex" => Some(ExecutorKind::Codex),
            "opencode" => Some(ExecutorKind::OpenCode),
            "aider" => Some(ExecutorKind::Aider),
            "goose" => Some(ExecutorKind::Goose),
            "qwen" => Some(ExecutorKind::Qwen),
            "cline" => Some(ExecutorKind::Cline),
            "crush" => Some(ExecutorKind::Crush),
            "amplifier" => Some(ExecutorKind::Amplifier),
            "octomind" => Some(ExecutorKind::Octomind),
            "dexto" => Some(ExecutorKind::Dexto),
            "pi" => Some(ExecutorKind::Pi),
            _ => None,
        }
    }

    /// Whether this executor needs an `EndpointConfig` resolved.
    pub fn needs_endpoint(self) -> bool {
        matches!(self, ExecutorKind::Native)
    }

    /// Whether this executor is an external CLI adapter addressed by an
    /// executor-name prefix (`opencode:…`, `aider:…`, …). Used by all the
    /// prefix-routing call sites (spawn-path route parsing, `handler_for_model`,
    /// profile activation, `wg add` model resolution).
    pub fn is_external_cli(self) -> bool {
        Self::EXTERNAL_CLIS.contains(&self)
    }

    /// Whether this executor is a worker-only external CLI adapter.
    ///
    /// These executors are valid for task-agent spawns through the worker
    /// path, but they do not implement WG's live chat/session protocol. Used
    /// to reject them from the live-chat path. `OpenCode` is NOT worker-only
    /// (it has `wg opencode-handler --chat`), so this returns `false` for it
    /// even though [`is_external_cli`](Self::is_external_cli) returns `true`.
    pub fn is_worker_only_external(self) -> bool {
        Self::WORKER_ONLY_EXTERNALS.contains(&self)
    }
}

/// Resolved model identity carried in a `SpawnPlan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelSpec {
    /// Original spec string as it was sourced (e.g. `"opus"` or
    /// `"openrouter:deepseek/deepseek-v3.2"`). Useful for logs.
    pub raw: String,
    /// Provider prefix if present (`Some("claude")`, `Some("openrouter")`,
    /// …). `None` for bare aliases like `"opus"` or `"haiku"`.
    pub provider: Option<String>,
    /// The model identifier portion. For bare aliases, this is the alias
    /// itself; for `provider:model`, it's the part after the colon.
    pub model_id: String,
}

impl ResolvedModelSpec {
    pub fn from_raw(raw: &str) -> Self {
        let parsed = parse_model_spec(raw);
        ResolvedModelSpec {
            raw: raw.to_string(),
            provider: parsed.provider,
            model_id: parsed.model_id,
        }
    }
}

/// Executor-qualified model route split from a per-task model string.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutorModelRoute {
    executor: ExecutorKind,
    model: String,
}

/// Parse worker-executor-qualified model routes such as
/// `opencode:openrouter/stepfun/step-3.7-flash`.
///
/// This is intentionally narrower than `parse_model_spec`: `opencode` is an
/// executor, not a model provider, so it must not be added to
/// `KNOWN_PROVIDERS`. The only accepted nested provider shorthand today is
/// OpenRouter's CLI spelling (`openrouter/<provider>/<model>`), which we
/// normalize to WG's internal `openrouter:<provider>/<model>` model spec.
fn parse_executor_model_route(raw: &str) -> Option<ExecutorModelRoute> {
    let (prefix, rest) = raw.split_once(':')?;
    let executor = ExecutorKind::from_str(prefix)?;
    if !executor.is_external_cli() || rest.trim().is_empty() {
        return None;
    }

    let model = if let Some(openrouter_model) = rest.strip_prefix("openrouter/") {
        format!("openrouter:{}", openrouter_model)
    } else {
        rest.to_string()
    };

    Some(ExecutorModelRoute { executor, model })
}

/// Records *where* each field of a `SpawnPlan` came from. Logged on every
/// spawn so silent routing is impossible.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnProvenance {
    /// e.g. `"task.exec_mode=shell"`, `"agent.effective_executor"`,
    /// `"local [dispatcher].executor"`, `"global [dispatcher].executor"`,
    /// `"default"`.
    pub executor_source: String,
    /// e.g. `"task.model"`, `"local [agent].model (alias)"`,
    /// `"role.assigner"`, `"default"`.
    pub model_source: String,
    /// e.g. `"none (executor=claude)"`, `"local [llm_endpoints] is_default"`,
    /// `"role.endpoint"`, `"none (no endpoints configured)"`.
    pub endpoint_source: String,
}

impl SpawnProvenance {
    /// Render a one-line provenance entry suitable for the daemon log.
    /// Prefix with `[dispatcher]` or `agent-N:` at the call site.
    pub fn log_line(&self, plan: &SpawnPlan) -> String {
        let endpoint_str = match &plan.endpoint {
            Some(ep) => format!("{} ({})", ep.name, self.endpoint_source),
            None => format!("none ({})", self.endpoint_source),
        };
        format!(
            "SpawnPlan executor={} (from {}), model={} (from {}), endpoint={}",
            plan.executor.as_str(),
            self.executor_source,
            plan.model.raw,
            self.model_source,
            endpoint_str,
        )
    }
}

/// The single struct describing what a task spawn launches.
#[derive(Debug, Clone)]
pub struct SpawnPlan {
    pub executor: ExecutorKind,
    pub model: ResolvedModelSpec,
    /// `None` for executors that handle their own endpoint (claude/codex/
    /// shell/external workers). `Some(_)` only for `executor=native`.
    pub endpoint: Option<EndpointConfig>,
    /// Environment variables to set on the spawned process. Plan-level only;
    /// the spawn-execution layer is free to add wrapper-internal vars
    /// (`WG_TASK_ID`, `WG_AGENT_ID`, …) on top.
    pub env: HashMap<String, String>,
    /// argv tokens (program + args). Empty until the spawn-execution layer
    /// has migrated to consume the plan; in the interim, callers may build
    /// argv from `executor` + `model` + `endpoint` and the existing
    /// per-executor templates.
    pub argv: Vec<String>,
    pub provenance: SpawnProvenance,
}

/// Build the canonical `SpawnPlan` for a task. **This is the only place
/// that decides which executor / model / endpoint a spawn uses.**
///
/// `agent_executor` is the agency-derived `effective_executor()` for the
/// task's bound agent (or `None` if the task has no agent / agency lookup
/// failed). `default_model` is the dispatcher's currently-resolved
/// task-agent model (already cascaded through tier/role/global).
pub fn plan_spawn(
    task: &Task,
    config: &Config,
    agent_executor: Option<&str>,
    default_model: Option<&str>,
) -> Result<SpawnPlan> {
    plan_spawn_with_provider_health(task, config, agent_executor, default_model, None)
}

/// Build a spawn plan while excluding providers currently marked unhealthy.
pub fn plan_spawn_with_provider_health(
    task: &Task,
    config: &Config,
    agent_executor: Option<&str>,
    default_model: Option<&str>,
    provider_health: Option<&ProviderHealth>,
) -> Result<SpawnPlan> {
    // ----- 1. Executor -----
    let (executor, executor_source) = resolve_executor(task, config, agent_executor);

    // ----- 2. Model -----
    // Per-task model wins over default. Both are kept verbatim — we don't
    // rewrite `opus` to `claude:opus` here because the model field's role is
    // to be passed to the executor, which knows how to resolve aliases.
    let (model_raw, model_source) = if let Some(m) = task.model.as_deref() {
        (m.to_string(), "task.model".to_string())
    } else if let Some(m) = default_model {
        (m.to_string(), "dispatcher.default_model".to_string())
    } else if let Some(m) = config.coordinator.model.as_deref() {
        (m.to_string(), "local [dispatcher].model".to_string())
    } else {
        (
            config.agent.model.clone(),
            "[agent].model (fallback)".to_string(),
        )
    };

    let (model_raw, model_source) = choose_available_model(
        task,
        config,
        default_model,
        executor,
        &executor_source,
        model_raw,
        model_source,
        provider_health,
    );

    let executor_route = parse_executor_model_route(&model_raw);
    let (model_raw, model_source) = if let Some(route) = executor_route.as_ref() {
        (
            route.model.clone(),
            format!("{} (executor-qualified route {})", model_source, model_raw),
        )
    } else {
        (model_raw, model_source)
    };

    // ----- 2a. Bare-route → OpenRouter normalization (nex only) -----
    // A bare `vendor/model` route on the Nex/native executor with NO explicit
    // endpoint is an OpenRouter route (nex-optional-openrouter-endpoint).
    // Normalize it to `openrouter:<route>` so endpoint resolution below and
    // the downstream nex provider target OpenRouter directly instead of
    // silently falling back to the local `is_default` endpoint. An explicit
    // endpoint (named or URL) keeps the model verbatim — the user picked the
    // route, so the endpoint's provider dictates it.
    let has_explicit_endpoint = task
        .endpoint
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let (model_raw, model_source) = if executor == ExecutorKind::Native && !has_explicit_endpoint {
        let normalized = crate::config::normalize_bare_openrouter_route(&model_raw);
        if normalized != model_raw {
            (
                normalized,
                format!("{} (bare route → openrouter default)", model_source),
            )
        } else {
            (model_raw, model_source)
        }
    } else {
        (model_raw, model_source)
    };

    let model = ResolvedModelSpec::from_raw(&model_raw);

    // ----- 2b. Model-route override -----
    // Provider-qualified model specs are the source of truth for the handler.
    // Legacy executor keys are retained as hints for bare aliases only.
    //
    // Shell is terminal: a task with `task.exec` set runs that command
    // verbatim with no model, so a provider-qualified default model (e.g.
    // `pi:zai/glm-5.2` once pi became the default executor) must NOT promote
    // an external-CLI executor over the explicit Shell resolution. Without
    // this guard, promoting any external-CLI executor (pi, opencode, ...) to
    // the default would route `task.exec` shell commands into the LLM.
    let (executor, executor_source) = enforce_model_compat(executor, executor_source, &model);
    let (executor, executor_source) = if executor == ExecutorKind::Shell {
        (executor, executor_source)
    } else if let Some(route) = executor_route {
        (
            route.executor,
            format!(
                "model-route override: task.model requested executor={} with inner model={}",
                route.executor.as_str(),
                model.raw
            ),
        )
    } else {
        (executor, executor_source)
    };
    validate_cli_backend_match(executor, &model)?;

    // ----- 3. Endpoint (executor-scoped) -----
    //
    // Precedence (highest first):
    //   1. `task.endpoint` — set by `wg add -e <url|name>`, or by the IPC
    //      `CreateChat` handler from the user's launcher choice.
    //      - http(s):// URL  → synthesized inline `EndpointConfig` (matches
    //        the `-e http(s)://...` shortcut at provider.rs:208-230).
    //        The name carries the URL itself; spawn_task.rs:233 forwards
    //        `plan.endpoint.name` as the `-e` arg, so `wg nex` receives
    //        the literal URL and routes through the inline-URL path.
    //      - bare name      → looked up via `find_by_name`.
    //   2. Named endpoint matching `agent_executor`-derived role (TODO).
    //   3. Global `is_default` endpoint.
    //
    // Without precedence step 1, IPC-created chats with a custom endpoint
    // (e.g. TUI new-chat dialog with `-e https://lambda01...`) silently
    // dropped the URL and fell back to whatever endpoint was marked
    // `is_default` in config — talking to the wrong server.
    // (fix-nex-chat / Fix C — diagnose-wg-nex root cause #2.)
    let (endpoint, endpoint_source) = if executor.needs_endpoint() {
        if let Some(ep_str) = task.endpoint.as_deref() {
            if ep_str.starts_with("http://") || ep_str.starts_with("https://") {
                let synth = EndpointConfig {
                    name: ep_str.to_string(),
                    provider: "oai-compat".to_string(),
                    url: Some(ep_str.to_string()),
                    model: None,
                    api_key: None,
                    api_key_file: None,
                    api_key_env: None,
                    api_key_ref: None,
                    is_default: false,
                    context_window: None,
                };
                (
                    Some(synth),
                    format!("task.endpoint (inline URL: {})", ep_str),
                )
            } else if let Some(ep) = config.llm_endpoints.find_by_name(ep_str) {
                (
                    Some(ep.clone()),
                    format!("task.endpoint (named: {})", ep_str),
                )
            } else if let Some(default_ep) = config.llm_endpoints.find_default() {
                (
                    Some(default_ep.clone()),
                    format!(
                        "[llm_endpoints] is_default (task.endpoint={:?} not found)",
                        ep_str
                    ),
                )
            } else {
                (
                    None,
                    format!("none (task.endpoint={:?} not found and no default)", ep_str),
                )
            }
        } else if crate::config::model_is_openrouter(&model.raw) {
            // No explicit endpoint + an OpenRouter model → route to OpenRouter
            // intentionally (nex-optional-openrouter-endpoint), NOT the local
            // `is_default` endpoint. Prefer a configured OpenRouter endpoint —
            // it carries the URL + API key, which the spawn path forwards by
            // name. With none configured, pass no endpoint and let the nex
            // provider resolve the openrouter.ai default from the model's
            // `openrouter:` prefix (still OpenRouter, never local).
            if let Some(or_ep) = config.llm_endpoints.find_for_provider("openrouter") {
                (
                    Some(or_ep.clone()),
                    "[llm_endpoints] provider=openrouter (openrouter model, no endpoint specified)"
                        .to_string(),
                )
            } else {
                (
                    None,
                    "none (openrouter model, no openrouter endpoint configured — \
                     nex uses the openrouter.ai default, not the local default)"
                        .to_string(),
                )
            }
        } else if let Some(default_ep) = config.llm_endpoints.find_default() {
            (
                Some(default_ep.clone()),
                "[llm_endpoints] is_default".to_string(),
            )
        } else {
            (
                None,
                "none (no endpoints configured for native)".to_string(),
            )
        }
    } else {
        (None, format!("none (executor={})", executor.as_str()))
    };

    // ----- 4. Env -----
    // Plan-level env: WG_EXECUTOR_TYPE + WG_MODEL are guaranteed correct
    // because they come from the same `executor` + `model` resolved above.
    // The spawn-execution layer adds wrapper-internal vars on top.
    let mut env = HashMap::new();
    env.insert(
        "WG_EXECUTOR_TYPE".to_string(),
        executor.as_str().to_string(),
    );
    env.insert("WG_MODEL".to_string(), model.raw.clone());

    let provenance = SpawnProvenance {
        executor_source,
        model_source,
        endpoint_source,
    };

    Ok(SpawnPlan {
        executor,
        model,
        endpoint,
        env,
        argv: Vec::new(),
        provenance,
    })
}

/// If the model spec carries a provider prefix, switch to the executor the
/// provider requires. Bare aliases like `opus` / `sonnet` keep the resolved
/// executor for backwards compatibility.
fn enforce_model_compat(
    executor: ExecutorKind,
    executor_source: String,
    model: &ResolvedModelSpec,
) -> (ExecutorKind, String) {
    if executor == ExecutorKind::Shell || executor.is_external_cli() {
        return (executor, executor_source);
    }
    let Some(ref provider) = model.provider else {
        return (executor, executor_source);
    };
    let required = provider_to_executor(provider);
    if required == executor.as_str() {
        return (executor, executor_source);
    }
    let Some(kind) = ExecutorKind::from_str(required) else {
        return (executor, executor_source);
    };
    let new_source = format!(
        "model-route override: was {} (from {}), model={} prefix={} requires {}",
        executor.as_str(),
        executor_source,
        model.raw,
        provider,
        required,
    );
    eprintln!(
        "[dispatch] model-route: {} (from {}) cannot run model '{}' (prefix '{}'); routing to '{}'",
        executor.as_str(),
        executor_source,
        model.raw,
        provider,
        required,
    );
    (kind, new_source)
}

fn choose_available_model(
    task: &Task,
    config: &Config,
    default_model: Option<&str>,
    base_executor: ExecutorKind,
    base_executor_source: &str,
    primary_model: String,
    primary_source: String,
    provider_health: Option<&ProviderHealth>,
) -> (String, String) {
    let Some(health) = provider_health else {
        return (primary_model, primary_source);
    };

    let candidates = model_candidates(task, config, default_model, &primary_model, &primary_source);
    for (candidate, source) in candidates {
        let (candidate_model, candidate_executor) =
            candidate_route(task, base_executor, base_executor_source, &candidate);
        let provider_id = extract_provider_id(candidate_executor.as_str(), Some(&candidate_model));
        if health.is_provider_available(&provider_id) {
            if candidate_model != primary_model {
                eprintln!(
                    "[dispatch] provider-health: route '{}' unavailable; using model '{}' from {}",
                    extract_provider_id(base_executor.as_str(), Some(&primary_model)),
                    candidate_model,
                    source
                );
            }
            return (candidate_model, source);
        }
    }

    (primary_model, primary_source)
}

fn model_candidates(
    task: &Task,
    config: &Config,
    default_model: Option<&str>,
    primary_model: &str,
    primary_source: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    push_model_candidate(
        &mut out,
        primary_model.to_string(),
        primary_source.to_string(),
    );
    if let Some(model) = task.model.as_deref() {
        push_model_candidate(&mut out, model.to_string(), "task.model".to_string());
    }
    if let Some(model) = default_model {
        push_model_candidate(
            &mut out,
            model.to_string(),
            "dispatcher.default_model".to_string(),
        );
    }
    push_model_candidate(
        &mut out,
        config
            .resolve_model_for_role(crate::config::DispatchRole::TaskAgent)
            .spawn_model_spec(),
        "models.task_agent".to_string(),
    );
    if let Some(model) = config.coordinator.model.as_deref() {
        push_model_candidate(
            &mut out,
            model.to_string(),
            "[dispatcher].model".to_string(),
        );
    }
    push_model_candidate(
        &mut out,
        config.agent.model.clone(),
        "[agent].model".to_string(),
    );
    for tier in [
        crate::config::Tier::Premium,
        crate::config::Tier::Standard,
        crate::config::Tier::Fast,
    ] {
        if let Some(resolved) = config.resolve_tier(tier) {
            push_model_candidate(
                &mut out,
                resolved.spawn_model_spec(),
                format!("tiers.{:?}", tier).to_ascii_lowercase(),
            );
        }
    }
    out
}

fn push_model_candidate(out: &mut Vec<(String, String)>, model: String, source: String) {
    if model.trim().is_empty() {
        return;
    }
    if out.iter().any(|(m, _)| m == &model) {
        return;
    }
    out.push((model, source));
}

fn candidate_route(
    task: &Task,
    base_executor: ExecutorKind,
    base_executor_source: &str,
    raw_model: &str,
) -> (String, ExecutorKind) {
    let executor_route = parse_executor_model_route(raw_model);
    let model_raw = executor_route
        .as_ref()
        .map(|route| route.model.clone())
        .unwrap_or_else(|| raw_model.to_string());
    let has_explicit_endpoint = task
        .endpoint
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let model_raw = if base_executor == ExecutorKind::Native && !has_explicit_endpoint {
        crate::config::normalize_bare_openrouter_route(&model_raw)
    } else {
        model_raw
    };
    let model = ResolvedModelSpec::from_raw(&model_raw);
    let (executor, _) =
        enforce_model_compat(base_executor, base_executor_source.to_string(), &model);
    let executor = executor_route
        .map(|route| route.executor)
        .unwrap_or(executor);
    (model_raw, executor)
}

/// Reject explicit CLI-backend mismatches before anything launches.
///
/// Native/OAI-compatible providers intentionally stay flexible because native
/// and external executors can speak more than one backend. CLI-backed
/// providers are different: the Claude CLI cannot run `codex:` models, and the
/// Codex CLI cannot run `claude:` models. Letting those pairs through produces
/// doomed agent exits with generic non-zero failure reasons.
fn validate_cli_backend_match(executor: ExecutorKind, model: &ResolvedModelSpec) -> Result<()> {
    let Some(provider) = model.provider.as_deref() else {
        return Ok(());
    };
    match (executor, provider) {
        (ExecutorKind::Claude, "codex") => Err(anyhow!(
            "backend-mismatch: executor={} cannot run model={} (provider prefix '{}' requires executor=codex)",
            executor.as_str(),
            model.raw,
            provider
        )),
        (ExecutorKind::Codex, "claude") => Err(anyhow!(
            "backend-mismatch: executor={} cannot run model={} (provider prefix '{}' requires executor=claude)",
            executor.as_str(),
            model.raw,
            provider
        )),
        _ => Ok(()),
    }
}

/// Resolve which executor kind to use for a task spawn, with provenance.
///
/// Precedence (highest first):
/// 1. `task.exec` set, or `task.exec_mode == "shell"`     →  Shell
/// 2. `task.exec_mode` parses to a known executor          →  that executor
/// 3. `agent_executor` (agency-derived effective executor) →  that executor
/// 4. `[dispatcher].executor` (local or global merged)     →  that executor
/// 5. Default                                              →  Claude
///
/// **Crucially: model is never consulted here.** The caller may have a
/// non-Anthropic model spec, but if the dispatcher is pinned to claude,
/// we honor claude. The previous implementation auto-switched to native
/// based on a model→provider lookup; that behavior is what this function
/// deliberately removes.
fn resolve_executor(
    task: &Task,
    config: &Config,
    agent_executor: Option<&str>,
) -> (ExecutorKind, String) {
    // 1. Shell beats everything: `task.exec` set or `exec_mode == "shell"`.
    if task.exec.is_some() {
        return (ExecutorKind::Shell, "task.exec set".to_string());
    }
    if task.exec_mode.as_deref() == Some("shell") {
        return (ExecutorKind::Shell, "task.exec_mode=shell".to_string());
    }

    // 2. Per-task exec_mode mapping to a known executor (rare).
    if let Some(mode) = task.exec_mode.as_deref()
        && let Some(kind) = ExecutorKind::from_str(mode)
    {
        return (kind, format!("task.exec_mode={}", mode));
    }

    // 3. Agency-derived effective executor.
    if let Some(exec) = agent_executor
        && let Some(kind) = ExecutorKind::from_str(exec)
    {
        return (kind, "agency.effective_executor".to_string());
    }

    // 4. Local/global [dispatcher].executor.
    if let Some(ref exec) = config.coordinator.executor
        && let Some(kind) = ExecutorKind::from_str(exec)
    {
        return (kind, "[dispatcher].executor".to_string());
    }

    // 5. Default.
    (ExecutorKind::Claude, "default".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EndpointConfig;
    use crate::graph::Task;

    fn base_task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            title: id.to_string(),
            ..Task::default()
        }
    }

    fn openrouter_default_endpoint() -> EndpointConfig {
        EndpointConfig {
            name: "openrouter".to_string(),
            provider: "openrouter".to_string(),
            url: Some("https://openrouter.ai/api/v1".to_string()),
            model: None,
            api_key: None,
            api_key_file: None,
            api_key_env: Some("OPENROUTER_API_KEY".to_string()),
            api_key_ref: None,
            is_default: true,
            context_window: None,
        }
    }

    fn external_worker_cases() -> Vec<(&'static str, ExecutorKind)> {
        ExecutorKind::EXTERNAL_CLIS
            .iter()
            .copied()
            .map(|kind| (kind.as_str(), kind))
            .collect()
    }

    #[test]
    fn test_executor_kind_recognizes_external_workers() {
        for (name, kind) in external_worker_cases() {
            assert_eq!(
                ExecutorKind::from_str(name),
                Some(kind),
                "ExecutorKind must parse first-class external executor '{}'",
                name
            );
            assert_eq!(kind.as_str(), name);
            assert!(
                kind.is_external_cli(),
                "{} must be classified as an external CLI",
                name
            );
            assert!(
                !kind.needs_endpoint(),
                "{} must not inherit native endpoint requirements",
                name
            );
        }
    }

    #[test]
    fn test_chat_capable_external_clis_are_not_worker_only() {
        for kind in [
            ExecutorKind::OpenCode,
            ExecutorKind::Octomind,
            ExecutorKind::Dexto,
            ExecutorKind::Pi,
        ] {
            assert!(
                kind.is_external_cli(),
                "{} is an external CLI",
                kind.as_str()
            );
            assert!(
                !kind.is_worker_only_external(),
                "{} is chat-capable and must not be classified worker-only",
                kind.as_str()
            );
        }

        // The remaining external CLIs are still worker-only (no chat handler).
        for kind in [
            ExecutorKind::Aider,
            ExecutorKind::Goose,
            ExecutorKind::Qwen,
            ExecutorKind::Cline,
            ExecutorKind::Crush,
            ExecutorKind::Amplifier,
        ] {
            assert!(
                kind.is_external_cli(),
                "{} is an external CLI",
                kind.as_str()
            );
            assert!(
                kind.is_worker_only_external(),
                "{} is still worker-only (no live chat handler)",
                kind.as_str()
            );
        }
    }

    #[test]
    fn test_only_native_nex_needs_endpoint() {
        assert!(ExecutorKind::Native.needs_endpoint());
        assert_eq!(ExecutorKind::from_str("nex"), Some(ExecutorKind::Native));
        assert!(ExecutorKind::from_str("nex").unwrap().needs_endpoint());

        for kind in [
            ExecutorKind::Claude,
            ExecutorKind::Codex,
            ExecutorKind::Shell,
            ExecutorKind::OpenCode,
            ExecutorKind::Aider,
            ExecutorKind::Goose,
            ExecutorKind::Qwen,
            ExecutorKind::Cline,
            ExecutorKind::Crush,
            ExecutorKind::Amplifier,
            ExecutorKind::Octomind,
            ExecutorKind::Dexto,
            ExecutorKind::Pi,
        ] {
            assert!(
                !kind.needs_endpoint(),
                "{} must not require EndpointConfig",
                kind.as_str()
            );
        }
    }

    #[test]
    fn test_dispatcher_external_executor_survives_plan_spawn() {
        for (name, kind) in external_worker_cases() {
            let mut config = Config::default();
            config.coordinator.executor = Some(name.to_string());
            config
                .llm_endpoints
                .endpoints
                .push(openrouter_default_endpoint());

            let mut task = base_task("t1");
            task.endpoint = Some("openrouter".to_string());

            let plan = plan_spawn(
                &task,
                &config,
                None,
                Some("openrouter:deepseek/deepseek-v3.2"),
            )
            .unwrap();

            assert_eq!(
                plan.executor, kind,
                "dispatcher.executor={} must survive plan_spawn; provenance={:?}",
                name, plan.provenance
            );
            assert_eq!(
                plan.env.get("WG_EXECUTOR_TYPE").map(String::as_str),
                Some(name),
                "SpawnPlan env must preserve the external executor name"
            );
            assert!(
                plan.endpoint.is_none(),
                "{} must not receive EndpointConfig even when task/default endpoints exist",
                name
            );
            assert!(
                plan.provenance.endpoint_source.contains(name),
                "endpoint provenance should explain the external executor policy, got {:?}",
                plan.provenance.endpoint_source
            );
        }
    }

    #[test]
    fn test_agency_external_executor_survives_plan_spawn() {
        for (name, kind) in external_worker_cases() {
            let mut config = Config::default();
            config.coordinator.executor = Some("claude".to_string());

            let task = base_task("t1");
            let plan = plan_spawn(&task, &config, Some(name), Some("claude:opus")).unwrap();

            assert_eq!(
                plan.executor, kind,
                "agency executor {} must beat dispatcher default and survive plan_spawn",
                name
            );
            assert_eq!(plan.provenance.executor_source, "agency.effective_executor");
            assert_eq!(
                plan.env.get("WG_EXECUTOR_TYPE").map(String::as_str),
                Some(name)
            );
        }
    }

    #[test]
    fn test_shell_still_beats_external_dispatcher_executor() {
        let mut config = Config::default();
        config.coordinator.executor = Some("opencode".to_string());

        let mut task = base_task("t1");
        task.exec = Some("echo hello".to_string());

        let plan = plan_spawn(&task, &config, None, Some("claude:opus")).unwrap();
        assert_eq!(plan.executor, ExecutorKind::Shell);
        assert!(plan.provenance.executor_source.contains("task.exec"));
    }

    /// THE regression test. Reproduces the exact scenario that bit the user:
    /// task model `opus`, global config has `is_default = openrouter`, and
    /// local `[dispatcher].executor = claude`. The previous implementation
    /// would auto-switch to `native` because `opus` could be resolved via
    /// the openrouter endpoint. The contract: executor wins, period.
    #[test]
    fn test_executor_floor_is_honored() {
        let mut config = Config::default();
        config.coordinator.executor = Some("claude".to_string());
        config
            .llm_endpoints
            .endpoints
            .push(openrouter_default_endpoint());

        let mut task = base_task("t1");
        task.model = Some("opus".to_string());

        let plan = plan_spawn(&task, &config, None, Some("opus")).unwrap();

        assert_eq!(
            plan.executor,
            ExecutorKind::Claude,
            "executor MUST be claude when [dispatcher].executor=claude is explicit, regardless of model='opus' + global openrouter is_default. provenance: {:?}",
            plan.provenance
        );
        assert_eq!(plan.model.raw, "opus");
    }

    #[test]
    fn test_no_endpoint_for_claude_executor() {
        let mut config = Config::default();
        config.coordinator.executor = Some("claude".to_string());
        // Even with a global default endpoint configured, claude must not get one.
        config
            .llm_endpoints
            .endpoints
            .push(openrouter_default_endpoint());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("opus")).unwrap();

        assert_eq!(plan.executor, ExecutorKind::Claude);
        assert!(
            plan.endpoint.is_none(),
            "endpoint MUST be None for executor=claude, got {:?}. provenance: {:?}",
            plan.endpoint,
            plan.provenance
        );
        assert!(
            plan.provenance.endpoint_source.contains("executor=claude"),
            "endpoint_source must explain *why* there's no endpoint, got {:?}",
            plan.provenance.endpoint_source
        );
    }

    #[test]
    fn test_provenance_traces_every_field() {
        // Every field of SpawnPlan must have a non-empty provenance
        // explanation pointing to the config source that produced it.
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config.coordinator.model = Some("openrouter:deepseek/deepseek-v3.2".to_string());
        config
            .llm_endpoints
            .endpoints
            .push(openrouter_default_endpoint());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, None).unwrap();

        assert!(
            !plan.provenance.executor_source.is_empty(),
            "executor_source must be populated"
        );
        assert!(
            !plan.provenance.model_source.is_empty(),
            "model_source must be populated"
        );
        assert!(
            !plan.provenance.endpoint_source.is_empty(),
            "endpoint_source must be populated"
        );
        // Sanity: the chosen executor matches the local [dispatcher] override.
        assert_eq!(plan.executor, ExecutorKind::Native);
        assert!(plan.provenance.executor_source.contains("[dispatcher]"));
        assert_eq!(
            plan.endpoint.as_ref().map(|e| e.name.as_str()),
            Some("openrouter")
        );

        // The log line is what gets printed on every spawn — render it and
        // verify each field is mentioned.
        let line = plan.provenance.log_line(&plan);
        assert!(line.contains("executor=native"));
        assert!(line.contains("model=openrouter:deepseek/deepseek-v3.2"));
        assert!(line.contains("endpoint=openrouter"));
    }

    fn local_default_endpoint() -> EndpointConfig {
        EndpointConfig {
            name: "local-gpu".to_string(),
            provider: "local".to_string(),
            url: Some("http://127.0.0.1:8088/v1".to_string()),
            model: None,
            api_key: None,
            api_key_file: None,
            api_key_env: None,
            api_key_ref: None,
            is_default: true,
            context_window: None,
        }
    }

    fn nex_chat_task(model: &str) -> Task {
        let mut task = base_task(".chat-1");
        task.model = Some(model.to_string());
        task
    }

    // ── nex-optional-openrouter-endpoint: blank endpoint defaults to OpenRouter ──

    #[test]
    fn bare_vendor_model_nex_no_endpoint_normalizes_to_openrouter() {
        // The canonical TUI [+] flow: pick nex, type `minimax/minimax-m3`,
        // leave endpoint blank. The bare route must normalize to the
        // OpenRouter spec and MUST NOT carry the local is_default endpoint.
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config
            .llm_endpoints
            .endpoints
            .push(local_default_endpoint());

        let task = nex_chat_task("minimax/minimax-m3");
        let plan = plan_spawn(&task, &config, Some("native"), None).unwrap();

        assert_eq!(plan.executor, ExecutorKind::Native);
        assert_eq!(
            plan.model.raw, "openrouter:minimax/minimax-m3",
            "bare vendor/model on nex with no endpoint becomes an openrouter spec"
        );
        assert!(
            plan.endpoint.is_none(),
            "must NOT silently fall back to the local is_default endpoint; got {:?}",
            plan.endpoint.as_ref().map(|e| &e.name)
        );
        assert!(
            plan.provenance.endpoint_source.contains("openrouter"),
            "endpoint provenance should name the openrouter route: {}",
            plan.provenance.endpoint_source
        );
    }

    #[test]
    fn openrouter_model_nex_no_endpoint_does_not_fall_back_to_local_default() {
        // Explicit `openrouter:` prefix + blank endpoint + only a local
        // default configured → no local fallback.
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config
            .llm_endpoints
            .endpoints
            .push(local_default_endpoint());

        let task = nex_chat_task("openrouter:minimax/minimax-m3");
        let plan = plan_spawn(&task, &config, Some("native"), None).unwrap();

        assert_eq!(plan.model.raw, "openrouter:minimax/minimax-m3");
        assert!(
            plan.endpoint.is_none(),
            "openrouter model must not adopt the local default endpoint; got {:?}",
            plan.endpoint.as_ref().map(|e| &e.name)
        );
    }

    #[test]
    fn openrouter_model_nex_no_endpoint_prefers_configured_openrouter_endpoint() {
        // When an OpenRouter endpoint IS configured, a blank-endpoint
        // openrouter model routes through it (carries URL + API key).
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config
            .llm_endpoints
            .endpoints
            .push(local_default_endpoint());
        config
            .llm_endpoints
            .endpoints
            .push(openrouter_default_endpoint());

        let task = nex_chat_task("minimax/minimax-m3");
        let plan = plan_spawn(&task, &config, Some("native"), None).unwrap();

        assert_eq!(plan.model.raw, "openrouter:minimax/minimax-m3");
        assert_eq!(
            plan.endpoint.as_ref().map(|e| e.name.as_str()),
            Some("openrouter"),
            "should select the configured openrouter endpoint, not the local default"
        );
        assert_eq!(
            plan.endpoint.as_ref().and_then(|e| e.url.as_deref()),
            Some("https://openrouter.ai/api/v1")
        );
    }

    #[test]
    fn explicit_named_endpoint_keeps_model_verbatim_and_endpoint() {
        // Regression guard: the existing named-endpoint launch path must be
        // untouched. Picking `lambda01` keeps the bare model verbatim (the
        // endpoint dictates the route) and resolves that named endpoint.
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config.llm_endpoints.endpoints.push(EndpointConfig {
            name: "lambda01".to_string(),
            provider: "local".to_string(),
            url: Some("https://lambda01.example:30000".to_string()),
            model: None,
            api_key: None,
            api_key_file: None,
            api_key_env: None,
            api_key_ref: None,
            is_default: false,
            context_window: None,
        });

        let mut task = nex_chat_task("minimax/minimax-m3");
        task.endpoint = Some("lambda01".to_string());
        let plan = plan_spawn(&task, &config, Some("native"), None).unwrap();

        assert_eq!(
            plan.model.raw, "minimax/minimax-m3",
            "an explicit endpoint means the user chose the route; model stays bare"
        );
        assert_eq!(
            plan.endpoint.as_ref().map(|e| e.name.as_str()),
            Some("lambda01")
        );
    }

    #[test]
    fn bare_alias_without_slash_keeps_local_default_fallback() {
        // A bare alias WITHOUT a slash (e.g. a local model name) is not an
        // OpenRouter route — the historical local is_default fallback stays.
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config
            .llm_endpoints
            .endpoints
            .push(local_default_endpoint());

        let task = nex_chat_task("qwen3-coder-30b");
        let plan = plan_spawn(&task, &config, Some("native"), None).unwrap();

        assert_eq!(
            plan.model.raw, "qwen3-coder-30b",
            "no slash → not rewritten"
        );
        assert_eq!(
            plan.endpoint.as_ref().map(|e| e.name.as_str()),
            Some("local-gpu"),
            "bare local model still uses the configured default endpoint"
        );
    }

    #[test]
    fn test_shell_beats_dispatcher_executor() {
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());

        let mut task = base_task("t1");
        task.exec = Some("echo hello".to_string());

        let plan = plan_spawn(&task, &config, None, None).unwrap();
        assert_eq!(plan.executor, ExecutorKind::Shell);
        assert!(plan.provenance.executor_source.contains("task.exec"));
    }

    #[test]
    fn test_default_model_prefix_selects_executor() {
        let config = Config::default();
        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, None).unwrap();
        assert_eq!(
            plan.executor,
            ExecutorKind::Pi,
            "built-in default pi model should select the pi handler"
        );
        assert!(plan.provenance.executor_source.contains("model-route"));
    }

    #[test]
    fn test_agency_executor_overrides_dispatcher_default() {
        // When an agent_executor (agency-derived) is provided, it wins over
        // the dispatcher default but not over an explicit [dispatcher].executor.
        let config = Config::default();
        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, Some("native"), Some("opus")).unwrap();
        assert_eq!(plan.executor, ExecutorKind::Native);
        assert!(plan.provenance.executor_source.contains("agency"));
    }

    /// Stale executor pins must not beat provider-qualified models.
    #[test]
    fn test_local_model_overrides_stale_codex_executor_to_native() {
        let mut config = Config::default();
        config.coordinator.executor = Some("codex".to_string());
        config.coordinator.model = Some("local:qwen3-coder".to_string());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("local:qwen3-coder")).unwrap();

        assert_eq!(
            plan.executor,
            ExecutorKind::Native,
            "model={} must select native even when stale dispatcher.executor=codex. provenance: {:?}",
            plan.model.raw,
            plan.provenance
        );
    }

    /// Companion test: `wg init -x nex -m qwen3-coder` (canonicalised to
    /// `native`). Native executor with `local:` model is the entire reason
    /// you'd configure nex/native, so this is the must-not-break case.
    #[test]
    fn test_nex_executor_routes_native_not_claude() {
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config.coordinator.model = Some("local:qwen3-coder".to_string());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("local:qwen3-coder")).unwrap();

        assert_eq!(
            plan.executor,
            ExecutorKind::Native,
            "dispatcher.executor=native (nex) MUST be honored. provenance: {:?}",
            plan.provenance
        );
    }

    /// The autohaiku regression case, now enforced at the dispatch layer
    /// rather than agency: dispatcher.executor=claude (explicit) +
    /// model=local:qwen3-coder MUST switch to native, because the claude
    /// CLI literally cannot run a local model and would 404.
    #[test]
    fn test_claude_executor_with_local_model_overrides_to_native() {
        let mut config = Config::default();
        config.coordinator.executor = Some("claude".to_string());
        config.coordinator.model = Some("local:qwen3-coder".to_string());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("local:qwen3-coder")).unwrap();

        assert_eq!(
            plan.executor,
            ExecutorKind::Native,
            "claude CLI cannot run local: models — must override to native. provenance: {:?}",
            plan.provenance
        );
        assert!(
            plan.provenance.executor_source.contains("model-route"),
            "provenance must record the model-route override, got {:?}",
            plan.provenance.executor_source
        );
    }

    /// Default dispatcher (no executor configured) + agent default (claude)
    /// + local: model → must override to native. Same root cause as
    /// autohaiku, just driven by the default fall-through rather than an
    /// explicit `-x claude`.
    #[test]
    fn test_default_executor_with_local_model_overrides_to_native() {
        let mut config = Config::default();
        config.coordinator.model = Some("local:qwen3-coder".to_string());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("local:qwen3-coder")).unwrap();

        assert_eq!(
            plan.executor,
            ExecutorKind::Native,
            "default claude executor with local: model must override to native. provenance: {:?}",
            plan.provenance
        );
    }

    /// `claude:opus` (and bare `opus`) MUST NOT trigger an override — the
    /// claude CLI is the right choice for Anthropic models. This locks in
    /// the boundary: only non-Anthropic provider prefixes (local, openrouter,
    /// oai-compat, openai) trigger the model-compat switch.
    #[test]
    fn test_claude_executor_with_anthropic_model_no_override() {
        let mut config = Config::default();
        config.coordinator.executor = Some("claude".to_string());

        for model in ["opus", "sonnet", "claude:opus", "claude:sonnet"] {
            let task = base_task("t1");
            let plan = plan_spawn(&task, &config, None, Some(model)).unwrap();
            assert_eq!(
                plan.executor,
                ExecutorKind::Claude,
                "model={} must keep claude executor (no override). provenance: {:?}",
                model,
                plan.provenance
            );
        }
    }

    /// Provider-qualified local routes select native even when a stale codex
    /// executor key is present.
    #[test]
    fn test_codex_executor_with_local_model_routes_to_native() {
        let mut config = Config::default();
        config.coordinator.executor = Some("codex".to_string());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("local:qwen3-coder")).unwrap();
        assert_eq!(plan.executor, ExecutorKind::Native);
        assert!(
            plan.provenance.executor_source.contains("model-route"),
            "codex must be rewritten by provider:model routing. provenance: {:?}",
            plan.provenance
        );
    }

    #[test]
    fn test_codex_model_routes_to_codex_atomically() {
        let mut config = Config::default();
        config.coordinator.executor = Some("claude".to_string());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("codex:gpt-5.5")).unwrap();

        assert_eq!(
            plan.executor,
            ExecutorKind::Codex,
            "codex-class model must not remain paired with executor=claude. provenance: {:?}",
            plan.provenance
        );
        assert_eq!(plan.model.raw, "codex:gpt-5.5");
        assert_eq!(
            plan.env.get("WG_EXECUTOR_TYPE").map(String::as_str),
            Some("codex")
        );
        assert_eq!(
            plan.env.get("WG_MODEL").map(String::as_str),
            Some("codex:gpt-5.5")
        );
    }

    #[test]
    fn test_zai_model_overrides_stale_claude_executor_to_opencode() {
        let mut config = Config::default();
        config.coordinator.executor = Some("claude".to_string());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("zai:glm-5.1")).unwrap();

        assert_eq!(plan.executor, ExecutorKind::OpenCode);
        assert_eq!(plan.model.raw, "zai:glm-5.1");
        assert_eq!(
            plan.env.get("WG_EXECUTOR_TYPE").map(String::as_str),
            Some("opencode")
        );
    }

    #[test]
    fn test_provider_health_skips_paused_primary_model_route() {
        let mut config = Config::default();
        config.coordinator.executor = Some("claude".to_string());
        config.agent.model = "claude:opus".to_string();
        config.tiers.premium = Some("claude:opus".to_string());
        config.tiers.standard = Some("codex:gpt-5.5".to_string());

        let mut health = ProviderHealth::default();
        health.backoff_provider("claude", "session limit".to_string());

        let task = base_task("t1");
        let plan = plan_spawn_with_provider_health(
            &task,
            &config,
            None,
            Some("claude:opus"),
            Some(&health),
        )
        .unwrap();

        assert_eq!(plan.executor, ExecutorKind::Codex);
        assert_eq!(plan.model.raw, "codex:gpt-5.5");
        assert!(
            plan.provenance.model_source.contains("tiers.standard"),
            "fallback source should identify the alternate configured route: {:?}",
            plan.provenance
        );
    }

    #[test]
    fn test_opencode_openrouter_model_route_is_atomic() {
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config.coordinator.model = Some("openrouter:default/model".to_string());

        let mut task = base_task("t1");
        task.model = Some("opencode:openrouter/stepfun/step-3.7-flash".to_string());

        let plan = plan_spawn(
            &task,
            &config,
            Some("native"),
            Some("openrouter:default/model"),
        )
        .unwrap();

        assert_eq!(
            plan.executor,
            ExecutorKind::OpenCode,
            "opencode:openrouter/... must select the OpenCode executor atomically. provenance: {:?}",
            plan.provenance
        );
        assert_eq!(
            plan.model.raw, "openrouter:stepfun/step-3.7-flash",
            "inner OpenRouter model must be normalized for existing model/provider resolution"
        );
        assert_eq!(
            plan.env.get("WG_EXECUTOR_TYPE").map(String::as_str),
            Some("opencode")
        );
        assert_eq!(
            plan.env.get("WG_MODEL").map(String::as_str),
            Some("openrouter:stepfun/step-3.7-flash")
        );
        assert!(plan.endpoint.is_none());
        assert!(
            plan.provenance
                .executor_source
                .contains("model-route override"),
            "provenance should explain executor-qualified route, got {:?}",
            plan.provenance.executor_source
        );
    }

    /// Test C (fix-opencode-build): under the opencode profile, a `.chat-*`
    /// task resolves to the OpenCode handler. The profile sets
    /// `[dispatcher].model` (== `coordinator.model`) to the opencode route, so
    /// a chat/coordinator task with no per-task model override cascades to it
    /// and `parse_executor_model_route` flips the executor to OpenCode. This
    /// is the same config-driven path `wg profile use opencode` activates.
    #[test]
    fn test_chat_task_resolves_to_opencode_handler_under_opencode_profile() {
        let mut config = Config::default();
        // Emulate the active opencode profile's default chat/coordinator model.
        config.coordinator.model = Some("opencode:zai/glm-5.2".to_string());

        // A chat task carries no per-task model and no agency executor — it
        // must inherit the profile's opencode route.
        let task = base_task(".chat-foo");
        let plan = plan_spawn(&task, &config, None, None).unwrap();

        assert_eq!(
            plan.executor,
            ExecutorKind::OpenCode,
            "a .chat-* task under the opencode profile must dispatch via the \
             opencode handler; provenance={:?}",
            plan.provenance
        );

        // Goal #5 proper: opencode REPLACES claude as the default chat handler.
        // The daemon's CoordinatorAgent::start passes `executor=claude` by
        // default (`executor.unwrap_or("claude")`); the opencode route on
        // `coordinator.model` must still override that to OpenCode, otherwise a
        // default chat would stay on claude under the opencode profile.
        let plan_default_claude = plan_spawn(&task, &config, Some("claude"), None).unwrap();
        assert_eq!(
            plan_default_claude.executor,
            ExecutorKind::OpenCode,
            "the opencode coordinator.model route must override the daemon's \
             default executor=claude; provenance={:?}",
            plan_default_claude.provenance
        );
        // The inner model is normalized to the WG Z.ai CLI/provider route, and the
        // explicit-model contract means it is never empty.
        assert_eq!(plan.model.raw, "zai/glm-5.2");
        assert!(!plan.model.raw.is_empty());

        // And handler_for_model agrees on the opencode route (single source of
        // truth), so any direct caller routes identically.
        assert_eq!(
            crate::dispatch::handler_for_model("opencode:zai/glm-5.2"),
            ExecutorKind::OpenCode
        );
    }

    #[test]
    fn test_model_route_preserves_known_good_provider_routing() {
        let config = Config::default();
        let task = base_task("t1");

        let cases = [
            ("codex:gpt-5.5", ExecutorKind::Codex),
            ("claude:opus", ExecutorKind::Claude),
            ("nex:qwen3-coder", ExecutorKind::Native),
            ("openrouter:stepfun/step-3.7-flash", ExecutorKind::Native),
        ];

        for (model, expected_executor) in cases {
            let plan = plan_spawn(&task, &config, None, Some(model)).unwrap();
            assert_eq!(
                plan.executor, expected_executor,
                "model {} should route to {:?}; provenance={:?}",
                model, expected_executor, plan.provenance
            );
            assert_eq!(plan.model.raw, model);
        }
    }

    #[test]
    fn test_model_provider_resolves_backend_mismatch_before_spawn() {
        let mut config = Config::default();
        config.coordinator.executor = Some("codex".to_string());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("claude:opus"))
            .expect("model provider should pick a compatible handler before launch");

        assert_eq!(plan.executor, ExecutorKind::Claude);
        assert!(plan.provenance.executor_source.contains("model-route"));
    }

    /// Fix C regression-guard (fix-nex-chat / diagnose-wg-nex root cause #2):
    /// When a task has `task.endpoint` set to an http(s):// URL, plan_spawn
    /// MUST synthesize an `EndpointConfig` carrying that URL — NOT silently
    /// fall back to `find_default()`. Without this, IPC `CreateChat` with
    /// `endpoint=Some("https://lambda01...")` had its URL dropped on the
    /// floor: `wg spawn-task --dry-run .chat-N` emitted no `-e` flag.
    #[test]
    fn test_task_endpoint_inline_url_overrides_default() {
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config
            .llm_endpoints
            .endpoints
            .push(openrouter_default_endpoint());

        let mut task = base_task(".chat-32");
        task.model = Some("nex:qwen3-coder".to_string());
        task.endpoint = Some("https://lambda01.tail334fe6.ts.net:30000".to_string());

        let plan = plan_spawn(&task, &config, None, None).unwrap();

        assert_eq!(plan.executor, ExecutorKind::Native);
        let ep = plan.endpoint.as_ref().expect("endpoint must be Some");
        assert_eq!(
            ep.name, "https://lambda01.tail334fe6.ts.net:30000",
            "synthesized endpoint name must equal the inline URL so spawn_task forwards it as the -e arg"
        );
        assert_eq!(
            ep.url.as_deref(),
            Some("https://lambda01.tail334fe6.ts.net:30000")
        );
        assert!(
            plan.provenance.endpoint_source.contains("task.endpoint"),
            "endpoint_source must trace to task.endpoint, got {:?}",
            plan.provenance.endpoint_source
        );
        assert!(
            !plan.provenance.endpoint_source.contains("is_default"),
            "MUST NOT fall back to is_default when task.endpoint is set, got {:?}",
            plan.provenance.endpoint_source
        );
    }

    /// Fix C variant: `task.endpoint` referencing a configured endpoint by
    /// NAME (not URL) must look it up via `find_by_name`, not fall back to
    /// the default.
    #[test]
    fn test_task_endpoint_named_overrides_default() {
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config
            .llm_endpoints
            .endpoints
            .push(openrouter_default_endpoint());
        config.llm_endpoints.endpoints.push(EndpointConfig {
            name: "lambda01".to_string(),
            provider: "oai-compat".to_string(),
            url: Some("https://lambda01.example:30000".to_string()),
            model: None,
            api_key: None,
            api_key_file: None,
            api_key_env: None,
            api_key_ref: None,
            is_default: false,
            context_window: None,
        });

        let mut task = base_task(".chat-1");
        task.endpoint = Some("lambda01".to_string());

        let plan = plan_spawn(&task, &config, None, Some("nex:qwen3-coder")).unwrap();

        let ep = plan.endpoint.as_ref().expect("endpoint must be Some");
        assert_eq!(ep.name, "lambda01");
        assert_eq!(ep.url.as_deref(), Some("https://lambda01.example:30000"));
        assert!(
            plan.provenance.endpoint_source.contains("named"),
            "endpoint_source should record the named-lookup path, got {:?}",
            plan.provenance.endpoint_source
        );
    }

    /// Fix C boundary: when no `task.endpoint` is set, behaviour is
    /// unchanged — the default endpoint still wins. Locks in that the
    /// new branch is purely additive.
    #[test]
    fn test_no_task_endpoint_falls_back_to_default() {
        let mut config = Config::default();
        config.coordinator.executor = Some("native".to_string());
        config
            .llm_endpoints
            .endpoints
            .push(openrouter_default_endpoint());

        let task = base_task("t1");
        let plan = plan_spawn(&task, &config, None, Some("nex:qwen3-coder")).unwrap();

        let ep = plan.endpoint.as_ref().expect("endpoint must be Some");
        assert_eq!(ep.name, "openrouter");
        assert!(
            plan.provenance.endpoint_source.contains("is_default"),
            "with no task.endpoint, fall back to is_default. got {:?}",
            plan.provenance.endpoint_source
        );
    }

    /// Fix C boundary: claude executor must continue to ignore task.endpoint
    /// (the claude CLI handles its own auth/url). Per the precedence rules
    /// at the top of this file, executor=claude → endpoint=None always.
    #[test]
    fn test_claude_executor_ignores_task_endpoint() {
        let mut config = Config::default();
        config.coordinator.executor = Some("claude".to_string());

        let mut task = base_task("t1");
        task.endpoint = Some("https://anything.example".to_string());

        let plan = plan_spawn(&task, &config, None, Some("opus")).unwrap();
        assert_eq!(plan.executor, ExecutorKind::Claude);
        assert!(
            plan.endpoint.is_none(),
            "executor=claude must produce no endpoint, even with task.endpoint set"
        );
    }
}
