//! Serialization of [`OpenCodeConfig`](super::types::OpenCodeConfig) into the
//! JSON format expected by the OpenCode server process.
//!
//! The OpenCode server expects a JSON config with field names that differ from
//! our internal config (e.g. `instructions` → `prompt`, `max_turns` → `maxSteps`).
//! This module defines typed serde structs that express those mappings via
//! `#[serde(rename)]` attributes, keeping the serialization self-documenting
//! and eliminating manual `serde_json::Map` construction.

use serde::Serialize;
use std::collections::HashMap;
use tracing::warn;

use super::types::OpenCodeConfig;

/// Default provider used when no explicit provider is configured.
const DEFAULT_PROVIDER: &str = "z.ai";

// ─── Typed structs for OpenCode server JSON ───

/// Top-level config sent to the OpenCode server.
///
/// Field names match the OpenCode server's expected JSON schema.
/// Optional fields use `skip_serializing_if` so that empty/absent
/// values produce no key in the output (matching the previous manual
/// `Map::insert` behaviour).
#[derive(Serialize)]
struct OpenCodeServerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<HashMap<String, AgentConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp: Option<HashMap<String, McpServerEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<HashMap<String, ProviderEntry>>,
}

/// Per-agent config in the OpenCode server JSON.
///
/// Maps internal field names to the server's expected names:
/// - `instructions` → `prompt`
/// - `max_turns` → `maxSteps`
#[derive(Serialize)]
struct AgentConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// Renamed from `instructions`.
    #[serde(skip_serializing_if = "Option::is_none", rename = "prompt")]
    instructions: Option<String>,
    /// Renamed from `max_turns`.
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxSteps")]
    max_turns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<String>>,
}

/// Per-MCP-server entry in the OpenCode server JSON.
///
/// The `command` is an array of `[binary, arg1, arg2, …]`.
/// `env` is renamed to `environment`.
#[derive(Serialize)]
struct McpServerEntry {
    /// Always `"local"` for local MCP servers.
    #[serde(rename = "type")]
    server_type: String,
    /// `[command, …args]`
    command: Vec<String>,
    /// Renamed from `env`.
    #[serde(skip_serializing_if = "Option::is_none", rename = "environment")]
    env: Option<HashMap<String, String>>,
}

/// Provider entry containing API key options.
#[derive(Serialize)]
struct ProviderEntry {
    options: ProviderOptions,
}

/// Options nested inside a provider entry.
#[derive(Serialize)]
struct ProviderOptions {
    #[serde(rename = "apiKey")]
    api_key: String,
}

// ─── Conversion logic ───

impl OpenCodeServerConfig {
    /// Build a typed [`OpenCodeServerConfig`] from the internal [`OpenCodeConfig`],
    /// applying field-name mappings and resolving environment variables for API keys.
    fn from_config(config: &OpenCodeConfig) -> Self {
        let default_provider = config.model.provider.as_deref().unwrap_or(DEFAULT_PROVIDER);

        // Model: convert { id, provider } → "provider/model" string
        let model = if !config.model.id.is_empty() {
            Some(format!("{}/{}", default_provider, config.model.id))
        } else {
            None
        };

        // Agents → agent: map field names
        let agent = if !config.agents.is_empty() {
            let mapped: HashMap<String, AgentConfig> = config
                .agents
                .iter()
                .map(|(name, agent_cfg)| {
                    (name.clone(), AgentConfig::from_agent(agent_cfg, default_provider))
                })
                .collect();
            Some(mapped)
        } else {
            None
        };

        // MCP servers → mcp
        let mcp = if !config.mcp_servers.is_empty() {
            let mapped: HashMap<String, McpServerEntry> = config
                .mcp_servers
                .iter()
                .map(|(name, server_cfg)| {
                    (name.clone(), McpServerEntry::from_server(server_cfg))
                })
                .collect();
            Some(mapped)
        } else {
            None
        };

        // API key → provider config
        // WARNING: The JSON built here may contain API keys in the
        // `provider.<name>.options.apiKey` field. Never log the full output of
        // this function (or the config struct) at any level — it would leak secrets.
        let provider = resolve_provider_config(config, default_provider);

        Self {
            model,
            agent,
            mcp,
            provider,
        }
    }
}

impl AgentConfig {
    fn from_agent(agent_cfg: &super::types::OpenCodeAgentConfig, default_provider: &str) -> Self {
        // Model: if it contains '/', use as-is; otherwise prepend the provider.
        let model = agent_cfg.model.as_ref().map(|model| {
            if model.contains('/') {
                model.clone()
            } else {
                format!("{}/{}", default_provider, model)
            }
        });

        Self {
            model,
            instructions: agent_cfg.instructions.clone(),
            max_turns: agent_cfg.max_turns,
            disable: agent_cfg.disable,
            tools: agent_cfg.tools.clone(),
        }
    }
}

impl McpServerEntry {
    fn from_server(server_cfg: &super::types::OpenCodeMcpServerConfig) -> Self {
        let mut command = vec![server_cfg.command.clone()];
        if let Some(ref args) = server_cfg.args {
            command.extend(args.iter().cloned());
        }

        Self {
            server_type: "local".to_string(),
            command,
            env: server_cfg.env.clone(),
        }
    }
}

/// Resolve the provider config section containing the API key, if configured.
///
/// If `api_key_env` looks like an environment variable name (all uppercase,
/// contains `_`), the variable is resolved. If it doesn't exist, the provider
/// section is omitted entirely (with a warning). Literal values are used
/// directly.
fn resolve_provider_config(
    config: &OpenCodeConfig,
    default_provider: &str,
) -> Option<HashMap<String, ProviderEntry>> {
    config.model.api_key_env.as_ref().and_then(|api_key_env| {
        let raw = api_key_env.as_str();
        let api_key = if raw.starts_with(|c: char| c.is_ascii_uppercase()) && raw.contains('_') {
            match std::env::var(raw) {
                Ok(key) => key,
                Err(_) => {
                    warn!(
                        "Environment variable '{}' referenced in opencode.model.api_key_env is not set — \
                         API key will be missing from provider config",
                        raw
                    );
                    return None;
                }
            }
        } else {
            raw.to_string()
        };

        if api_key.is_empty() {
            return None;
        }

        let mut provider_map = HashMap::new();
        provider_map.insert(
            default_provider.to_string(),
            ProviderEntry {
                options: ProviderOptions { api_key },
            },
        );
        Some(provider_map)
    })
}

// ─── Public API ───

/// Build the JSON config string to pass via `OPENCODE_CONFIG_CONTENT`.
///
/// Converts the internal [`OpenCodeConfig`] into the JSON format expected by the
/// OpenCode server, applying field-name mappings and resolving API key
/// environment variables.
pub fn build_opencode_config_json(config: &OpenCodeConfig) -> String {
    let server_config = OpenCodeServerConfig::from_config(config);
    serde_json::to_string(&server_config).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{OpenCodeAgentConfig, OpenCodeMcpServerConfig, OpenCodeModelConfig};

    fn empty_config() -> OpenCodeConfig {
        OpenCodeConfig {
            model: OpenCodeModelConfig {
                id: String::new(),
                provider: None,
                api_key_env: None,
            },
            hostname: String::new(),
            port: 0,
            agents: HashMap::new(),
            mcp_servers: HashMap::new(),
            request_timeout_secs: 0,
            sse_max_retries: 50,
        }
    }

    // --- Basic / existing coverage ---

    #[test]
    fn build_config_empty_returns_empty_object() {
        let config = empty_config();
        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, serde_json::json!({}));
    }

    #[test]
    fn build_config_includes_model_string() {
        let mut config = empty_config();
        config.model.id = "glm-5-turbo".to_string();
        config.model.provider = Some("z.ai".to_string());

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["model"], "z.ai/glm-5-turbo");
    }

    #[test]
    fn build_config_uses_default_provider_when_missing() {
        let mut config = empty_config();
        config.model.id = "some-model".to_string();

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["model"], "z.ai/some-model");
    }

    #[test]
    fn build_config_valid_json_output() {
        let config = OpenCodeConfig::default();
        let json = build_opencode_config_json(&config);
        let _: serde_json::Value = serde_json::from_str(&json).expect("JSON should be valid");
    }

    // --- Comprehensive tests: multiple agents ---

    #[test]
    fn build_config_multiple_agents() {
        let mut config = empty_config();

        let mut agents = HashMap::new();
        agents.insert(
            "coder".to_string(),
            OpenCodeAgentConfig {
                model: Some("anthropic/claude-3".to_string()),
                instructions: Some("Write code".to_string()),
                tools: Some(vec!["read".to_string(), "bash".to_string()]),
                max_turns: Some(10),
                disable: Some(false),
            },
        );
        agents.insert(
            "reviewer".to_string(),
            OpenCodeAgentConfig {
                model: Some("anthropic/claude-3".to_string()),
                instructions: Some("Review code".to_string()),
                tools: Some(vec!["read".to_string(), "grep".to_string()]),
                max_turns: Some(5),
                disable: Some(false),
            },
        );
        agents.insert(
            "disabled-agent".to_string(),
            OpenCodeAgentConfig {
                model: None,
                instructions: None,
                tools: None,
                max_turns: None,
                disable: Some(true),
            },
        );
        config.agents = agents;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // All three agents should be present
        assert!(parsed["agent"]["coder"].is_object());
        assert!(parsed["agent"]["reviewer"].is_object());
        assert!(parsed["agent"]["disabled-agent"].is_object());

        // Verify coder fields
        assert_eq!(parsed["agent"]["coder"]["model"], "anthropic/claude-3");
        assert_eq!(parsed["agent"]["coder"]["prompt"], "Write code");
        assert_eq!(parsed["agent"]["coder"]["maxSteps"], 10);
        assert_eq!(parsed["agent"]["coder"]["disable"], false);
        let coder_tools = parsed["agent"]["coder"]["tools"].as_array().unwrap();
        assert_eq!(coder_tools.len(), 2);
        assert_eq!(coder_tools[0], "read");
        assert_eq!(coder_tools[1], "bash");

        // Verify reviewer fields
        assert_eq!(parsed["agent"]["reviewer"]["model"], "anthropic/claude-3");
        assert_eq!(parsed["agent"]["reviewer"]["prompt"], "Review code");
        assert_eq!(parsed["agent"]["reviewer"]["maxSteps"], 5);
        let reviewer_tools = parsed["agent"]["reviewer"]["tools"].as_array().unwrap();
        assert_eq!(reviewer_tools.len(), 2);

        // Verify disabled agent has disable=true
        assert_eq!(parsed["agent"]["disabled-agent"]["disable"], true);
    }

    // --- Comprehensive tests: agents with model mapping ---

    #[test]
    fn build_config_includes_agents_with_field_mapping() {
        let mut config = empty_config();

        let mut agents = HashMap::new();
        agents.insert(
            "coder".to_string(),
            OpenCodeAgentConfig {
                model: Some("anthropic/claude-3".to_string()),
                instructions: Some("Write code".to_string()),
                tools: None,
                max_turns: Some(10),
                disable: Some(false),
            },
        );
        config.agents = agents;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed["agent"]["coder"].is_object());
        assert_eq!(parsed["agent"]["coder"]["model"], "anthropic/claude-3");
        // "instructions" is mapped to "prompt" in the JSON
        assert_eq!(parsed["agent"]["coder"]["prompt"], "Write code");
        // "max_turns" is mapped to "maxSteps"
        assert_eq!(parsed["agent"]["coder"]["maxSteps"], 10);
        assert_eq!(parsed["agent"]["coder"]["disable"], false);
    }

    #[test]
    fn build_config_agent_model_without_provider_uses_default() {
        let mut config = empty_config();
        config.model.provider = Some("anthropic".to_string());

        let mut agents = HashMap::new();
        agents.insert(
            "simple".to_string(),
            OpenCodeAgentConfig {
                model: Some("claude-3".to_string()), // no "/" → uses config provider
                instructions: None,
                tools: Some(vec!["read".to_string(), "grep".to_string()]),
                max_turns: None,
                disable: Some(false),
            },
        );
        config.agents = agents;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Model without "/" uses the config's provider
        assert_eq!(parsed["agent"]["simple"]["model"], "anthropic/claude-3");
        let tools = parsed["agent"]["simple"]["tools"].as_array().unwrap();
        assert_eq!(tools[0], "read");
        assert_eq!(tools[1], "grep");
    }

    #[test]
    fn build_config_agent_minimal_model_only() {
        let mut config = empty_config();

        let mut agents = HashMap::new();
        agents.insert(
            "minimal".to_string(),
            OpenCodeAgentConfig {
                model: Some("anthropic/claude-3".to_string()),
                instructions: None,
                tools: None,
                max_turns: None,
                disable: Some(false),
            },
        );
        config.agents = agents;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Only model should be present, no prompt/maxSteps/tools
        assert_eq!(parsed["agent"]["minimal"]["model"], "anthropic/claude-3");
        assert!(parsed["agent"]["minimal"].get("prompt").is_none());
        assert!(parsed["agent"]["minimal"].get("maxSteps").is_none());
        assert!(parsed["agent"]["minimal"].get("tools").is_none());
    }

    #[test]
    fn build_config_agent_all_fields_including_tools() {
        let mut config = empty_config();
        config.model.id = "glm-5-turbo".to_string();
        config.model.provider = Some("z.ai".to_string());

        let mut agents = HashMap::new();
        agents.insert(
            "full-agent".to_string(),
            OpenCodeAgentConfig {
                model: Some("openai/gpt-4".to_string()),
                instructions: Some("You are an expert programmer.".to_string()),
                tools: Some(vec![
                    "read".to_string(),
                    "write".to_string(),
                    "bash".to_string(),
                    "glob".to_string(),
                    "grep".to_string(),
                ]),
                max_turns: Some(50),
                disable: Some(false),
            },
        );
        config.agents = agents;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Model should be present
        assert_eq!(parsed["model"], "z.ai/glm-5-turbo");

        // Agent with all fields
        let agent = &parsed["agent"]["full-agent"];
        assert_eq!(agent["model"], "openai/gpt-4");
        assert_eq!(agent["prompt"], "You are an expert programmer.");
        assert_eq!(agent["maxSteps"], 50);
        assert_eq!(agent["disable"], false);

        let tools = agent["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 5);
        assert_eq!(tools[0], "read");
        assert_eq!(tools[1], "write");
        assert_eq!(tools[2], "bash");
        assert_eq!(tools[3], "glob");
        assert_eq!(tools[4], "grep");
    }

    // --- Comprehensive tests: MCP servers ---

    #[test]
    fn build_config_includes_mcp_servers() {
        let mut config = empty_config();

        let mut mcp_servers = HashMap::new();
        mcp_servers.insert(
            "filesystem".to_string(),
            OpenCodeMcpServerConfig {
                command: "npx".to_string(),
                args: Some(vec![
                    "-y".to_string(),
                    "@anthropic/mcp-server".to_string(),
                ]),
                env: None,
            },
        );
        config.mcp_servers = mcp_servers;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed["mcp"]["filesystem"].is_object());
        assert_eq!(parsed["mcp"]["filesystem"]["type"], "local");
        let cmd = parsed["mcp"]["filesystem"]["command"].as_array().unwrap();
        assert_eq!(cmd[0], "npx");
        assert_eq!(cmd[1], "-y");
        assert_eq!(cmd[2], "@anthropic/mcp-server");
    }

    #[test]
    fn build_config_multiple_mcp_servers() {
        let mut config = empty_config();

        let mut mcp_servers = HashMap::new();
        mcp_servers.insert(
            "filesystem".to_string(),
            OpenCodeMcpServerConfig {
                command: "npx".to_string(),
                args: Some(vec![
                    "-y".to_string(),
                    "@anthropic/mcp-server".to_string(),
                ]),
                env: None,
            },
        );
        mcp_servers.insert(
            "github".to_string(),
            OpenCodeMcpServerConfig {
                command: "npx".to_string(),
                args: Some(vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-github".to_string(),
                ]),
                env: None,
            },
        );

        let mut env = HashMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "ghp_test".to_string());
        mcp_servers.insert(
            "postgres".to_string(),
            OpenCodeMcpServerConfig {
                command: "npx".to_string(),
                args: Some(vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-postgres".to_string(),
                    "postgres://localhost/db".to_string(),
                ]),
                env: Some(env),
            },
        );
        config.mcp_servers = mcp_servers;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // All three MCP servers should be present
        assert!(parsed["mcp"]["filesystem"].is_object());
        assert!(parsed["mcp"]["github"].is_object());
        assert!(parsed["mcp"]["postgres"].is_object());

        // Verify filesystem server
        assert_eq!(parsed["mcp"]["filesystem"]["type"], "local");
        let fs_cmd = parsed["mcp"]["filesystem"]["command"].as_array().unwrap();
        assert_eq!(fs_cmd[0], "npx");
        assert_eq!(fs_cmd[1], "-y");
        assert_eq!(fs_cmd[2], "@anthropic/mcp-server");

        // Verify github server
        assert_eq!(parsed["mcp"]["github"]["type"], "local");
        let gh_cmd = parsed["mcp"]["github"]["command"].as_array().unwrap();
        assert_eq!(gh_cmd[0], "npx");

        // Verify postgres server has environment
        assert_eq!(
            parsed["mcp"]["postgres"]["environment"]["GITHUB_TOKEN"],
            "ghp_test"
        );
        let pg_cmd = parsed["mcp"]["postgres"]["command"].as_array().unwrap();
        assert_eq!(pg_cmd.len(), 4); // "npx" + 3 args
        assert_eq!(pg_cmd[3], "postgres://localhost/db");
    }

    #[test]
    fn build_config_mcp_server_with_environment() {
        let mut config = empty_config();

        let mut env = HashMap::new();
        env.insert("API_KEY".to_string(), "secret123".to_string());

        let mut mcp_servers = HashMap::new();
        mcp_servers.insert(
            "custom".to_string(),
            OpenCodeMcpServerConfig {
                command: "python".to_string(),
                args: Some(vec!["server.py".to_string()]),
                env: Some(env),
            },
        );
        config.mcp_servers = mcp_servers;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(
            parsed["mcp"]["custom"]["environment"]["API_KEY"],
            "secret123"
        );
    }

    #[test]
    fn build_config_mcp_server_without_args() {
        let mut config = empty_config();

        let mut mcp_servers = HashMap::new();
        mcp_servers.insert(
            "simple".to_string(),
            OpenCodeMcpServerConfig {
                command: "custom-binary".to_string(),
                args: None, // No args
                env: None,
            },
        );
        config.mcp_servers = mcp_servers;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Command should be a single-element array
        assert!(parsed["mcp"]["simple"].is_object());
        assert_eq!(parsed["mcp"]["simple"]["type"], "local");
        let cmd = parsed["mcp"]["simple"]["command"].as_array().unwrap();
        assert_eq!(cmd.len(), 1);
        assert_eq!(cmd[0], "custom-binary");
        // Environment should not be present
        assert!(parsed["mcp"]["simple"].get("environment").is_none());
    }

    // --- Comprehensive tests: API key / provider config ---

    #[test]
    fn build_config_api_key_from_env_var() {
        let mut config = empty_config();
        config.model.id = "test-model".to_string();
        config.model.provider = Some("test-provider".to_string());
        config.model.api_key_env = Some("NONEXISTENT_ENV_VAR_FOR_TESTING".to_string());

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Since the env var doesn't exist, it resolves to empty string,
        // and provider config should NOT be included
        assert!(parsed.get("provider").is_none());
    }

    #[test]
    fn build_config_api_key_literal_value() {
        let mut config = empty_config();
        config.model.id = "test-model".to_string();
        config.model.provider = Some("myprovider".to_string());
        config.model.api_key_env = Some("sk-12345literal".to_string());

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Literal value (not uppercase + underscore pattern) is used directly
        assert_eq!(
            parsed["provider"]["myprovider"]["options"]["apiKey"],
            "sk-12345literal"
        );
    }

    #[test]
    fn build_config_api_key_from_actual_env_var() {
        // Set a temporary env var for this test
        std::env::set_var(
            "CORTEX_TEST_API_KEY_FOR_BUILD_CONFIG",
            "test-secret-key-12345",
        );

        let mut config = empty_config();
        config.model.id = "test-model".to_string();
        config.model.provider = Some("test-provider".to_string());
        config.model.api_key_env = Some("CORTEX_TEST_API_KEY_FOR_BUILD_CONFIG".to_string());

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // The env var exists and should be resolved
        assert_eq!(
            parsed["provider"]["test-provider"]["options"]["apiKey"],
            "test-secret-key-12345"
        );

        // Cleanup
        std::env::remove_var("CORTEX_TEST_API_KEY_FOR_BUILD_CONFIG");
    }

    // --- Comprehensive tests: combined / complex configs ---

    #[test]
    fn build_config_complex_all_fields_combined() {
        let mut config = empty_config();
        config.model.id = "glm-5-turbo".to_string();
        config.model.provider = Some("z.ai".to_string());
        config.model.api_key_env = Some("sk-test-complex-key".to_string());

        let mut agents = HashMap::new();
        agents.insert(
            "coder".to_string(),
            OpenCodeAgentConfig {
                model: Some("anthropic/claude-3".to_string()),
                instructions: Some("You are a coding assistant".to_string()),
                tools: Some(vec![
                    "read".to_string(),
                    "write".to_string(),
                    "bash".to_string(),
                ]),
                max_turns: Some(20),
                disable: Some(false),
            },
        );
        config.agents = agents;

        let mut mcp_servers = HashMap::new();
        mcp_servers.insert(
            "filesystem".to_string(),
            OpenCodeMcpServerConfig {
                command: "npx".to_string(),
                args: Some(vec![
                    "-y".to_string(),
                    "@anthropic/mcp-server".to_string(),
                ]),
                env: None,
            },
        );
        config.mcp_servers = mcp_servers;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Model should be present
        assert_eq!(parsed["model"], "z.ai/glm-5-turbo");

        // Agent should be present with correct fields
        assert!(parsed["agent"]["coder"].is_object());
        assert_eq!(parsed["agent"]["coder"]["model"], "anthropic/claude-3");
        assert_eq!(parsed["agent"]["coder"]["prompt"], "You are a coding assistant");
        assert_eq!(parsed["agent"]["coder"]["maxSteps"], 20);
        let tools = parsed["agent"]["coder"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);

        // MCP server should be present
        assert!(parsed["mcp"]["filesystem"].is_object());
        assert_eq!(parsed["mcp"]["filesystem"]["type"], "local");

        // Provider with API key should be present
        assert_eq!(
            parsed["provider"]["z.ai"]["options"]["apiKey"],
            "sk-test-complex-key"
        );

        // Verify all top-level keys exist
        assert!(parsed.get("model").is_some());
        assert!(parsed.get("agent").is_some());
        assert!(parsed.get("mcp").is_some());
        assert!(parsed.get("provider").is_some());
    }

    // --- Comprehensive tests: empty optional fields omitted ---

    #[test]
    fn build_config_empty_optional_fields_omitted() {
        let config = empty_config();
        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // All optional sections should be absent
        assert!(
            parsed.get("model").is_none(),
            "model should be omitted when id is empty"
        );
        assert!(
            parsed.get("agent").is_none(),
            "agent should be omitted when empty"
        );
        assert!(
            parsed.get("mcp").is_none(),
            "mcp should be omitted when empty"
        );
        assert!(
            parsed.get("provider").is_none(),
            "provider should be omitted when no api_key_env"
        );

        // Should just be an empty object
        assert_eq!(parsed, serde_json::json!({}));
    }

    #[test]
    fn build_config_model_only_no_other_fields() {
        let mut config = empty_config();
        config.model.id = "glm-5-turbo".to_string();
        config.model.provider = Some("z.ai".to_string());

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Only model should be present
        assert_eq!(parsed["model"], "z.ai/glm-5-turbo");
        assert_eq!(parsed.as_object().unwrap().len(), 1);
        assert!(parsed.get("agent").is_none());
        assert!(parsed.get("mcp").is_none());
        assert!(parsed.get("provider").is_none());
    }
}
