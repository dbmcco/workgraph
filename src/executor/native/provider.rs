//! Provider trait and model-based routing.
//!
//! The `Provider` trait abstracts over LLM API wire formats (Anthropic Messages,
//! OpenAI Chat Completions). Implementations handle headers, request/response
//! serialization, and tool call encoding while the agent loop works with a
//! uniform interface.
//!
//! Use `create_provider()` to route a model string to the appropriate backend:
//! - Bare name (`claude-sonnet-4-5-20250514`) → Anthropic native API
//! - Prefixed (`openai/gpt-4o`, `deepseek/deepseek-chat-v3`) → OpenAI-compatible

use std::path::Path;

use anyhow::{Context, Result};

use super::client::{AnthropicClient, MessagesRequest, MessagesResponse};
use super::openai_client::OpenAiClient;

/// Provider-agnostic LLM client trait.
///
/// Both `AnthropicClient` and `OpenAiClient` implement this trait so the
/// agent loop can work with any backend without knowing wire format details.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Provider name for logging (e.g., "anthropic", "openai").
    fn name(&self) -> &str;

    /// The model this provider is configured with.
    fn model(&self) -> &str;

    /// Maximum tokens per response.
    fn max_tokens(&self) -> u32;

    /// Send a completion request and return the response.
    ///
    /// The provider translates between the canonical message format and
    /// its wire protocol.
    async fn send(&self, request: &MessagesRequest) -> Result<MessagesResponse>;
}

/// Create a provider by routing on the model string.
///
/// - Bare model name (no `/`) → Anthropic native API
/// - Model with `/` prefix (e.g., `openai/gpt-4o`) → OpenAI-compatible API
/// - Explicit provider override via config or `WG_LLM_PROVIDER` env var
///
/// Reads `[native_executor]` section from `config.toml` for `provider`,
/// `api_base`, and `max_tokens` settings.
pub fn create_provider(workgraph_dir: &Path, model: &str) -> Result<Box<dyn Provider>> {
    let config_path = workgraph_dir.join("config.toml");
    let config_val: Option<toml::Value> = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|c| toml::from_str(&c).ok());

    let native_cfg = config_val.as_ref().and_then(|v| v.get("native_executor"));

    // Resolve provider: config > env var > model heuristic
    let provider_name = native_cfg
        .and_then(|c| c.get("provider"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| std::env::var("WG_LLM_PROVIDER").ok())
        .unwrap_or_else(|| {
            if model.contains('/') {
                "openai".to_string()
            } else {
                "anthropic".to_string()
            }
        });

    let api_base = native_cfg
        .and_then(|c| c.get("api_base"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let max_tokens = native_cfg
        .and_then(|c| c.get("max_tokens"))
        .and_then(|v| v.as_integer())
        .map(|v| v as u32);

    match provider_name.as_str() {
        "openai" | "openrouter" => {
            let mut client = OpenAiClient::from_env(model)
                .or_else(|_| {
                    let key = super::client::resolve_api_key_from_dir(workgraph_dir)?;
                    OpenAiClient::new(key, model, None)
                })
                .context("Failed to initialize OpenAI-compatible client")?;
            if let Some(base) = api_base {
                client = client.with_base_url(&base);
            }
            if let Some(mt) = max_tokens {
                client = client.with_max_tokens(mt);
            }
            eprintln!(
                "[native-exec] Using OpenAI-compatible provider ({})",
                client.model
            );
            Ok(Box::new(client))
        }
        _ => {
            let mut client = AnthropicClient::from_env(model)
                .context("Failed to initialize Anthropic client")?;
            if let Some(base) = api_base {
                client = client.with_base_url(&base);
            }
            if let Some(mt) = max_tokens {
                client = client.with_max_tokens(mt);
            }
            eprintln!("[native-exec] Using Anthropic provider ({})", client.model);
            Ok(Box::new(client))
        }
    }
}
