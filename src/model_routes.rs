use std::env;
use std::path::{Path, PathBuf};

const REGISTRY_ENV: &str = "PAIA_MODEL_ROUTE_REGISTRY_PATH";

pub const WORKGRAPH_OPENROUTER_FAST_ROUTE: &str = "workgraph.openrouter_fast";
pub const WORKGRAPH_OPENROUTER_STANDARD_ROUTE: &str = "workgraph.openrouter_standard";
pub const WORKGRAPH_OPENROUTER_PREMIUM_ROUTE: &str = "workgraph.openrouter_premium";
pub const WORKGRAPH_CLAUDE_CLI_FAST_ROUTE: &str = "workgraph.claude_cli_fast";
pub const WORKGRAPH_CLAUDE_CLI_STANDARD_ROUTE: &str = "workgraph.claude_cli_standard";
pub const WORKGRAPH_CLAUDE_CLI_PREMIUM_ROUTE: &str = "workgraph.claude_cli_premium";
pub const WORKGRAPH_CODEX_CLI_FAST_ROUTE: &str = "workgraph.codex_cli_fast";
pub const WORKGRAPH_CODEX_CLI_STANDARD_ROUTE: &str = "workgraph.codex_cli_standard";
pub const WORKGRAPH_CODEX_CLI_PREMIUM_ROUTE: &str = "workgraph.codex_cli_premium";
pub const WORKGRAPH_LOCAL_DEFAULT_ROUTE: &str = "workgraph.local_default";
pub const WORKGRAPH_CUSTOM_PLACEHOLDER_ROUTE: &str = "workgraph.custom_placeholder";

pub fn model_for_route(route_id: &str) -> String {
    let registry = load_registry();
    registry
        .get("model_routes")
        .and_then(|routes| routes.get(route_id))
        .and_then(|route| route.get("model"))
        .and_then(|model| model.as_str())
        .filter(|model| !model.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("Central model route {route_id} is missing a model"))
}

pub fn spec_for_route(provider_prefix: &str, route_id: &str) -> String {
    format!("{}:{}", provider_prefix, model_for_route(route_id))
}

fn load_registry() -> toml::Value {
    let path = registry_path().unwrap_or_else(|| {
        panic!("Unable to find central model route registry. Set {REGISTRY_ENV}.")
    });
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("Failed to read central model route registry {path:?}: {err}")
    });
    toml::from_str(&raw).unwrap_or_else(|err| {
        panic!("Failed to parse central model route registry {path:?}: {err}")
    })
}

fn registry_path() -> Option<PathBuf> {
    if let Ok(configured) = env::var(REGISTRY_ENV) {
        let configured = configured.trim();
        if !configured.is_empty() {
            let path = PathBuf::from(configured);
            if path.exists() {
                return Some(path);
            }
        }
    }

    let cwd = env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        if let Some(path) = sibling_registry_path(ancestor) {
            return Some(path);
        }
    }
    None
}

fn sibling_registry_path(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join("paia-agent-runtime/config/cognition-presets.toml");
    if direct.exists() {
        return Some(direct);
    }
    let sibling = dir
        .parent()
        .map(|parent| parent.join("paia-agent-runtime/config/cognition-presets.toml"))?;
    sibling.exists().then_some(sibling)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workgraph_routes_preserve_current_defaults() {
        assert_eq!(
            model_for_route(WORKGRAPH_OPENROUTER_FAST_ROUTE),
            "anthropic/claude-haiku-4-5"
        );
        assert_eq!(
            model_for_route(WORKGRAPH_OPENROUTER_STANDARD_ROUTE),
            "anthropic/claude-sonnet-4-6"
        );
        assert_eq!(
            model_for_route(WORKGRAPH_OPENROUTER_PREMIUM_ROUTE),
            "anthropic/claude-opus-4-7"
        );
        assert_eq!(model_for_route(WORKGRAPH_CLAUDE_CLI_FAST_ROUTE), "haiku");
        assert_eq!(
            model_for_route(WORKGRAPH_CLAUDE_CLI_STANDARD_ROUTE),
            "sonnet"
        );
        assert_eq!(model_for_route(WORKGRAPH_CLAUDE_CLI_PREMIUM_ROUTE), "opus");
        assert_eq!(
            model_for_route(WORKGRAPH_CODEX_CLI_FAST_ROUTE),
            "gpt-5-mini"
        );
        assert_eq!(model_for_route(WORKGRAPH_CODEX_CLI_STANDARD_ROUTE), "gpt-5");
        assert_eq!(model_for_route(WORKGRAPH_CODEX_CLI_PREMIUM_ROUTE), "o1-pro");
        assert_eq!(
            model_for_route(WORKGRAPH_LOCAL_DEFAULT_ROUTE),
            "qwen2.5-coder:7b"
        );
        assert_eq!(
            model_for_route(WORKGRAPH_CUSTOM_PLACEHOLDER_ROUTE),
            "custom-model"
        );
    }

    #[test]
    fn specs_use_workgraph_provider_prefixes() {
        assert_eq!(
            spec_for_route("openrouter", WORKGRAPH_OPENROUTER_STANDARD_ROUTE),
            "openrouter:anthropic/claude-sonnet-4-6"
        );
        assert_eq!(
            spec_for_route("codex", WORKGRAPH_CODEX_CLI_FAST_ROUTE),
            "codex:gpt-5-mini"
        );
    }
}
