//! Native executor CLI entry point.
//!
//! `wg native-exec` runs the Rust-native LLM agent loop for a task.
//! It is called by the spawn wrapper script when the executor type is "native".
//!
//! This command:
//! 1. Reads the prompt from a file
//! 2. Resolves the bundle for the exec_mode (tool filtering)
//! 3. Initializes the appropriate LLM client (Anthropic or OpenAI-compatible)
//! 4. Runs the agent loop to completion
//! 5. Exits with 0 on success, non-zero on failure

use std::path::Path;

use anyhow::{Context, Result};

use workgraph::config::Config;
use workgraph::executor::native::agent::AgentLoop;
use workgraph::executor::native::bundle::resolve_bundle;
use workgraph::executor::native::client::{AnthropicClient, LlmClient};
use workgraph::executor::native::openai_client::OpenAiClient;
use workgraph::executor::native::tools::ToolRegistry;

const DEFAULT_MODEL: &str = "claude-sonnet-4-5-20250514";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedNativeClientConfig {
    provider: String,
    api_base: Option<String>,
    api_key: Option<String>,
    max_tokens: Option<u32>,
}

fn resolve_legacy_native_executor_settings(
    workgraph_dir: &Path,
) -> (Option<String>, Option<String>, Option<String>, Option<u32>) {
    let config_path = workgraph_dir.join("config.toml");
    let config_val: Option<toml::Value> = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|content| toml::from_str(&content).ok());
    let native_cfg = config_val
        .as_ref()
        .and_then(|value| value.get("native_executor"));

    let provider = native_cfg
        .and_then(|cfg| cfg.get("provider"))
        .and_then(|value| value.as_str())
        .map(String::from);
    let api_base = native_cfg
        .and_then(|cfg| cfg.get("api_base"))
        .and_then(|value| value.as_str())
        .map(String::from);
    let api_key = native_cfg
        .and_then(|cfg| cfg.get("api_key"))
        .and_then(|value| value.as_str())
        .map(String::from)
        .filter(|value| !value.trim().is_empty());
    let max_tokens = native_cfg
        .and_then(|cfg| cfg.get("max_tokens"))
        .and_then(|value| value.as_integer())
        .map(|value| value as u32);

    (provider, api_base, api_key, max_tokens)
}

fn resolve_native_client_config(workgraph_dir: &Path, model: &str) -> ResolvedNativeClientConfig {
    let config = Config::load_or_default(workgraph_dir);
    let (legacy_provider, legacy_api_base, legacy_api_key, max_tokens) =
        resolve_legacy_native_executor_settings(workgraph_dir);

    let provider = std::env::var("WG_LLM_PROVIDER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or(legacy_provider)
        .unwrap_or_else(|| {
            if model.contains('/') {
                "openai".to_string()
            } else {
                "anthropic".to_string()
            }
        });

    let endpoint = config
        .llm_endpoints
        .find_for_provider(&provider)
        .or_else(|| config.llm_endpoints.find_default());

    let api_base = std::env::var("WG_ENDPOINT_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| endpoint.and_then(|ep| ep.url.clone()))
        .or(legacy_api_base);

    let api_key = std::env::var("WG_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| endpoint.and_then(|ep| ep.resolve_api_key().ok().flatten()))
        .or(legacy_api_key);

    ResolvedNativeClientConfig {
        provider,
        api_base,
        api_key,
        max_tokens,
    }
}

/// Resolve which LLM provider to use and create the appropriate client.
///
/// Resolution order:
/// 1. Spawn-resolved `WG_*` endpoint env vars
/// 2. Matching/default `[llm_endpoints]` config
/// 3. Legacy `[native_executor]` config
/// 4. Model heuristic / provider-specific env fallback
fn create_client(workgraph_dir: &Path, model: &str) -> Result<Box<dyn LlmClient>> {
    let resolved = resolve_native_client_config(workgraph_dir, model);

    match resolved.provider.as_str() {
        "openai" | "openrouter" => {
            let api_key = resolved
                .api_key
                .clone()
                .or_else(|| {
                    workgraph::executor::native::openai_client::resolve_openai_api_key_from_dir(
                        workgraph_dir,
                    )
                    .ok()
                })
                .or_else(|| {
                    workgraph::executor::native::client::resolve_api_key_from_dir(workgraph_dir)
                        .ok()
                })
                .context("Failed to initialize OpenAI-compatible client")?;
            let mut client = OpenAiClient::new(api_key, model, resolved.api_base.as_deref())
                .context("Failed to initialize OpenAI-compatible client")?;
            if let Some(mt) = resolved.max_tokens {
                client = client.with_max_tokens(mt);
            }
            eprintln!(
                "[native-exec] Using OpenAI-compatible provider ({})",
                client.model
            );
            Ok(Box::new(client))
        }
        _ => {
            let api_key = resolved
                .api_key
                .clone()
                .or_else(|| {
                    workgraph::executor::native::client::resolve_api_key_from_dir(workgraph_dir)
                        .ok()
                })
                .context("Failed to initialize Anthropic client")?;
            let mut client = AnthropicClient::from_config(&api_key, model)
                .context("Failed to initialize Anthropic client")?;
            if let Some(base) = resolved.api_base {
                client = client.with_base_url(&base);
            }
            if let Some(mt) = resolved.max_tokens {
                client = client.with_max_tokens(mt);
            }
            eprintln!("[native-exec] Using Anthropic provider ({})", client.model);
            Ok(Box::new(client))
        }
    }
}

/// Run the native executor agent loop.
pub fn run(
    workgraph_dir: &Path,
    prompt_file: &str,
    exec_mode: &str,
    task_id: &str,
    model: Option<&str>,
    max_turns: usize,
) -> Result<()> {
    let prompt = std::fs::read_to_string(prompt_file)
        .with_context(|| format!("Failed to read prompt file: {}", prompt_file))?;

    let effective_model = model
        .map(String::from)
        .or_else(|| std::env::var("WG_MODEL").ok())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    // Resolve the working directory (parent of .workgraph/)
    let working_dir = workgraph_dir
        .canonicalize()
        .ok()
        .and_then(|p| p.parent().map(|pp| pp.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    // Build the tool registry
    let mut registry = ToolRegistry::default_all(workgraph_dir, &working_dir);

    // Resolve bundle and filter tools
    let system_suffix = if let Some(bundle) = resolve_bundle(exec_mode, workgraph_dir) {
        let suffix = bundle.system_prompt_suffix.clone();
        registry = bundle.filter_registry(registry);
        suffix
    } else {
        String::new()
    };

    // Build full system prompt
    let system_prompt = if system_suffix.is_empty() {
        prompt
    } else {
        format!("{}\n\n{}", prompt, system_suffix)
    };

    // Build output log path
    let output_log = if let Ok(agent_id) = std::env::var("WG_AGENT_ID") {
        workgraph_dir
            .join("agents")
            .join(&agent_id)
            .join("agent.ndjson")
    } else {
        workgraph_dir.join("native-exec.ndjson")
    };

    eprintln!(
        "[native-exec] Starting agent loop for task '{}' with model '{}', exec_mode '{}', max_turns {}",
        task_id, effective_model, exec_mode, max_turns
    );

    // Create the API client (auto-selects provider)
    let client = create_client(workgraph_dir, &effective_model)?;

    // Create and run the agent loop
    let agent = AgentLoop::new(client, registry, system_prompt, max_turns, output_log);

    // Run the async agent loop
    let rt = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
    let result = rt.block_on(agent.run(&format!(
        "You are working on task '{}'. Complete the task as described in your system prompt. \
         When done, use the wg_done tool with task_id '{}'. \
         If you cannot complete the task, use the wg_fail tool with a reason.",
        task_id, task_id
    )))?;

    eprintln!(
        "[native-exec] Agent completed: {} turns, {}+{} tokens",
        result.turns, result.total_usage.input_tokens, result.total_usage.output_tokens
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_native_client_config;
    use serial_test::serial;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn native_client_config_prefers_spawn_env_over_legacy_config() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"
[native_executor]
provider = "anthropic"
api_base = "https://anthropic.example"
api_key = "sk-legacy"
max_tokens = 321
"#,
        )
        .unwrap();

        let saved_provider = std::env::var("WG_LLM_PROVIDER").ok();
        let saved_url = std::env::var("WG_ENDPOINT_URL").ok();
        let saved_key = std::env::var("WG_API_KEY").ok();
        unsafe {
            std::env::set_var("WG_LLM_PROVIDER", "openrouter");
            std::env::set_var("WG_ENDPOINT_URL", "https://router.example/v1");
            std::env::set_var("WG_API_KEY", "sk-spawn");
        }

        let resolved = resolve_native_client_config(temp_dir.path(), "openrouter:qwen/qwen3");

        assert_eq!(resolved.provider, "openrouter");
        assert_eq!(
            resolved.api_base.as_deref(),
            Some("https://router.example/v1")
        );
        assert_eq!(resolved.api_key.as_deref(), Some("sk-spawn"));
        assert_eq!(resolved.max_tokens, Some(321));

        match saved_provider {
            Some(value) => unsafe { std::env::set_var("WG_LLM_PROVIDER", value) },
            None => unsafe { std::env::remove_var("WG_LLM_PROVIDER") },
        }
        match saved_url {
            Some(value) => unsafe { std::env::set_var("WG_ENDPOINT_URL", value) },
            None => unsafe { std::env::remove_var("WG_ENDPOINT_URL") },
        }
        match saved_key {
            Some(value) => unsafe { std::env::set_var("WG_API_KEY", value) },
            None => unsafe { std::env::remove_var("WG_API_KEY") },
        }
    }

    #[test]
    #[serial]
    fn native_client_config_uses_matching_endpoint_when_spawn_env_is_absent() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"
[llm_endpoints]
[[llm_endpoints.endpoints]]
name = "router"
provider = "openrouter"
url = "https://openrouter.ai/api/v1"
api_key = "sk-router"
is_default = true

[native_executor]
provider = "openrouter"
max_tokens = 111
"#,
        )
        .unwrap();

        let saved_provider = std::env::var("WG_LLM_PROVIDER").ok();
        let saved_url = std::env::var("WG_ENDPOINT_URL").ok();
        let saved_key = std::env::var("WG_API_KEY").ok();
        unsafe {
            std::env::remove_var("WG_LLM_PROVIDER");
            std::env::remove_var("WG_ENDPOINT_URL");
            std::env::remove_var("WG_API_KEY");
        }

        let resolved = resolve_native_client_config(temp_dir.path(), "openrouter:qwen/qwen3");

        assert_eq!(resolved.provider, "openrouter");
        assert_eq!(
            resolved.api_base.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(resolved.api_key.as_deref(), Some("sk-router"));
        assert_eq!(resolved.max_tokens, Some(111));

        match saved_provider {
            Some(value) => unsafe { std::env::set_var("WG_LLM_PROVIDER", value) },
            None => unsafe { std::env::remove_var("WG_LLM_PROVIDER") },
        }
        match saved_url {
            Some(value) => unsafe { std::env::set_var("WG_ENDPOINT_URL", value) },
            None => unsafe { std::env::remove_var("WG_ENDPOINT_URL") },
        }
        match saved_key {
            Some(value) => unsafe { std::env::set_var("WG_API_KEY", value) },
            None => unsafe { std::env::remove_var("WG_API_KEY") },
        }
    }
}
