//! Per-project OpenCode server manager.

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use std::collections::HashMap;
use tokio::process::{Child, Command};
use tokio::time::Duration;

use crate::config::types::OpenCodeConfig;

const INITIAL_WAIT: Duration = Duration::from_secs(2);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_START_RETRIES: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Manages an OpenCode server process for a single project.
pub struct OpenCodeServer {
    process: Option<Child>,
    url: String,
    http_client: reqwest::Client,
}

impl OpenCodeServer {
    pub fn new() -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            process: None,
            url: String::new(),
            http_client,
        })
    }

    /// Start the server with the given config and working directory.
    pub async fn start(&mut self, config: &OpenCodeConfig, working_dir: &str) -> Result<()> {
        let host = &config.hostname;
        let port = config.port;
        self.url = format!("http://{}:{}", host, port);

        let server_config_json = build_opencode_config_json(config);

        for attempt in 0..=MAX_START_RETRIES {
            if attempt > 0 {
                info!("Retrying server start (attempt {}/{})", attempt, MAX_START_RETRIES);
                tokio::time::sleep(RETRY_DELAY).await;
            }

            match self.spawn_server(host, port, &server_config_json, working_dir).await {
                Ok(()) => match self.wait_for_healthy().await {
                    Ok(()) => {
                        info!(
                            "Server healthy on {} (pid: {:?})",
                            self.url,
                            self.process.as_ref().map(|p| p.id())
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        warn!("Server failed health check (attempt {}): {}", attempt + 1, e);
                        self.kill_process().await;
                    }
                },
                Err(e) => {
                    warn!("Failed to spawn server (attempt {}): {}", attempt + 1, e);
                }
            }
        }

        anyhow::bail!("Server failed to start after {} attempts", MAX_START_RETRIES + 1);
    }

    async fn spawn_server(
        &mut self,
        host: &str,
        port: u16,
        config_json: &str,
        working_dir: &str,
    ) -> Result<()> {
        info!("Spawning: opencode serve --hostname={} --port={} (cwd: {})", host, port, working_dir);

        let child = Command::new("opencode")
            .arg("serve")
            .arg(format!("--hostname={}", host))
            .arg(format!("--port={}", port))
            .env("OPENCODE_CONFIG_CONTENT", config_json)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("Failed to spawn 'opencode serve'. Is it installed?")?;

        self.process = Some(child);
        tokio::time::sleep(INITIAL_WAIT).await;
        Ok(())
    }

    async fn wait_for_healthy(&mut self) -> Result<()> {
        let health_url = format!("{}/app", self.url);
        let start = tokio::time::Instant::now();

        loop {
            if let Some(ref mut child) = self.process {
                if let Ok(Some(status)) = child.try_wait() {
                    anyhow::bail!("Server process exited prematurely with status: {}", status);
                }
            } else {
                anyhow::bail!("Server process is not running");
            }

            if start.elapsed() > HEALTH_TIMEOUT {
                anyhow::bail!("Server did not become healthy within {}s", HEALTH_TIMEOUT.as_secs());
            }

            match self.http_client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    debug!("Health check passed: GET {} → {}", health_url, resp.status());
                    return Ok(());
                }
                Ok(resp) => {
                    debug!("Health check not ready: GET {} → {}", health_url, resp.status());
                }
                Err(e) => {
                    debug!("Health check failed: GET {} → {}", health_url, e);
                }
            }

            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    /// Stop the server process.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.process.take() {
            info!("Stopping server...");
            match child.start_kill() {
                Ok(()) => debug!("Sent kill signal"),
                Err(e) => {
                    warn!("Failed to kill process: {}", e);
                    return Err(e).context("Failed to stop process");
                }
            }
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) => info!("Server exited with status: {}", status),
                Ok(Err(e)) => warn!("Error waiting for process: {}", e),
                Err(_) => warn!("Server did not exit within 5s"),
            }
        }
        Ok(())
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(ref mut child) = self.process {
            matches!(child.try_wait(), Ok(None))
        } else {
            false
        }
    }

    async fn kill_process(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        }
    }
}

/// Manages OpenCode servers for multiple projects.
pub struct ServerManager {
    servers: HashMap<String, OpenCodeServer>,
    base_port: u16,
    next_port_counter: u16,
}

impl ServerManager {
    pub fn new(base_port: u16) -> Self {
        Self {
            servers: HashMap::new(),
            base_port,
            next_port_counter: 0,
        }
    }

    /// Start a server for a project.
    pub async fn start_for_project(
        &mut self,
        project_id: &str,
        config: &mut OpenCodeConfig,
        working_dir: &str,
    ) -> Result<String> {
        let port = self.next_port(project_id);
        config.port = port;

        let mut server = OpenCodeServer::new()?;
        server.start(config, working_dir).await?;
        let url = server.url().to_string();

        self.servers.insert(project_id.to_string(), server);
        info!("Started server for project {} on {}", project_id, url);
        Ok(url)
    }

    /// Stop a project's server.
    pub async fn stop_for_project(&mut self, project_id: &str) -> Result<()> {
        if let Some(mut server) = self.servers.remove(project_id) {
            server.stop().await?;
        }
        Ok(())
    }

    /// Stop all servers.
    pub async fn stop_all(&mut self) {
        for (id, mut server) in self.servers.drain() {
            info!("Stopping server for project {}", id);
            let _ = server.stop().await;
        }
    }

    /// Get the URL for a project's server.
    pub fn get_url(&self, project_id: &str) -> Option<String> {
        self.servers.get(project_id).map(|s| s.url().to_string())
    }

    fn next_port(&mut self, _project_id: &str) -> u16 {
        let port = self.base_port + self.next_port_counter;
        self.next_port_counter += 1;
        port
    }
}

/// Build the JSON config string to pass via `OPENCODE_CONFIG_CONTENT`.
fn build_opencode_config_json(config: &OpenCodeConfig) -> String {
    let mut server_config = serde_json::Map::new();

    // Model: convert { id, provider } → "provider/model" string
    if !config.model.id.is_empty() {
        let provider = config.model.provider.as_deref().unwrap_or("z.ai");
        let model_str = format!("{}/{}", provider, config.model.id);
        server_config.insert("model".to_string(), serde_json::Value::String(model_str));
    }

    // Agents → agent: map field names
    if !config.agents.is_empty() {
        let mut mapped_agents = serde_json::Map::new();
        for (name, agent_cfg) in &config.agents {
            let mut mapped = serde_json::Map::new();
            if let Some(ref model) = agent_cfg.model {
                if model.contains('/') {
                    mapped.insert("model".to_string(), serde_json::Value::String(model.clone()));
                } else {
                    let provider = config.model.provider.as_deref().unwrap_or("z.ai");
                    mapped.insert("model".to_string(), serde_json::Value::String(format!("{}/{}", provider, model)));
                }
            }
            if let Some(ref instructions) = agent_cfg.instructions {
                mapped.insert("prompt".to_string(), serde_json::Value::String(instructions.clone()));
            }
            if let Some(max_turns) = agent_cfg.max_turns {
                mapped.insert("maxSteps".to_string(), serde_json::json!(max_turns));
            }
            if let Some(disable) = agent_cfg.disable {
                mapped.insert("disable".to_string(), serde_json::json!(disable));
            }
            if let Some(ref tools) = agent_cfg.tools {
                mapped.insert("tools".to_string(), serde_json::Value::Array(
                    tools.iter().map(|t| serde_json::Value::String(t.clone())).collect(),
                ));
            }
            mapped_agents.insert(name.clone(), serde_json::Value::Object(mapped));
        }
        server_config.insert("agent".to_string(), serde_json::Value::Object(mapped_agents));
    }

    // MCP servers → mcp
    if !config.mcp_servers.is_empty() {
        let mut mcp = serde_json::Map::new();
        for (name, server_cfg) in &config.mcp_servers {
            let mut entry = serde_json::Map::new();
            entry.insert("type".to_string(), serde_json::Value::String("local".to_string()));
            let mut command = vec![serde_json::Value::String(server_cfg.command.clone())];
            if let Some(ref args) = server_cfg.args {
                for arg in args {
                    command.push(serde_json::Value::String(arg.clone()));
                }
            }
            entry.insert("command".to_string(), serde_json::Value::Array(command));
            if let Some(ref env) = server_cfg.env {
                let mut env_map = serde_json::Map::new();
                for (k, v) in env {
                    env_map.insert(k.clone(), serde_json::Value::String(v.clone()));
                }
                entry.insert("environment".to_string(), serde_json::Value::Object(env_map));
            }
            mcp.insert(name.clone(), serde_json::Value::Object(entry));
        }
        server_config.insert("mcp".to_string(), serde_json::Value::Object(mcp));
    }

    // API key → provider config
    if let Some(ref api_key_env) = config.model.api_key_env {
        let raw = api_key_env.as_str();
        let api_key = if raw.starts_with(|c: char| c.is_ascii_uppercase()) && raw.contains('_') {
            std::env::var(raw).unwrap_or_default()
        } else {
            raw.to_string()
        };
        if !api_key.is_empty() {
            let provider = config.model.provider.as_deref().unwrap_or("z.ai");
            let mut provider_map = serde_json::Map::new();
            let mut options_map = serde_json::Map::new();
            options_map.insert("apiKey".to_string(), serde_json::Value::String(api_key));
            let mut provider_entry = serde_json::Map::new();
            provider_entry.insert("options".to_string(), serde_json::Value::Object(options_map));
            provider_map.insert(provider.to_string(), serde_json::Value::Object(provider_entry));
            server_config.insert("provider".to_string(), serde_json::Value::Object(provider_map));
        }
    }

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
        agents.insert("coder".to_string(), OpenCodeAgentConfig {
            model: Some("anthropic/claude-3".to_string()),
            instructions: Some("Write code".to_string()),
            tools: Some(vec!["read".to_string(), "bash".to_string()]),
            max_turns: Some(10),
            disable: Some(false),
            permission: None,
        });
        agents.insert("reviewer".to_string(), OpenCodeAgentConfig {
            model: Some("anthropic/claude-3".to_string()),
            instructions: Some("Review code".to_string()),
            tools: Some(vec!["read".to_string(), "grep".to_string()]),
            max_turns: Some(5),
            disable: Some(false),
            permission: None,
        });
        agents.insert("disabled-agent".to_string(), OpenCodeAgentConfig {
            model: None,
            instructions: None,
            tools: None,
            max_turns: None,
            disable: Some(true),
            permission: None,
        });
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
        agents.insert("coder".to_string(), OpenCodeAgentConfig {
            model: Some("anthropic/claude-3".to_string()),
            instructions: Some("Write code".to_string()),
            tools: None,
            max_turns: Some(10),
            disable: Some(false),
            permission: None,
        });
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
        agents.insert("simple".to_string(), OpenCodeAgentConfig {
            model: Some("claude-3".to_string()), // no "/" → uses default provider
            instructions: None,
            tools: Some(vec!["read".to_string(), "grep".to_string()]),
            max_turns: None,
            disable: Some(false),
            permission: None,
        });
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
        agents.insert("minimal".to_string(), OpenCodeAgentConfig {
            model: Some("anthropic/claude-3".to_string()),
            instructions: None,
            tools: None,
            max_turns: None,
            disable: Some(false),
            permission: None,
        });
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
        agents.insert("full-agent".to_string(), OpenCodeAgentConfig {
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
            permission: None,
        });
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
        mcp_servers.insert("filesystem".to_string(), OpenCodeMcpServerConfig {
            command: "npx".to_string(),
            args: Some(vec!["-y".to_string(), "@anthropic/mcp-server".to_string()]),
            env: None,
        });
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
        mcp_servers.insert("filesystem".to_string(), OpenCodeMcpServerConfig {
            command: "npx".to_string(),
            args: Some(vec!["-y".to_string(), "@anthropic/mcp-server".to_string()]),
            env: None,
        });
        mcp_servers.insert("github".to_string(), OpenCodeMcpServerConfig {
            command: "npx".to_string(),
            args: Some(vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()]),
            env: None,
        });

        let mut env = HashMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "ghp_test".to_string());
        mcp_servers.insert("postgres".to_string(), OpenCodeMcpServerConfig {
            command: "npx".to_string(),
            args: Some(vec!["-y".to_string(), "@modelcontextprotocol/server-postgres".to_string(), "postgres://localhost/db".to_string()]),
            env: Some(env),
        });
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
        assert_eq!(parsed["mcp"]["postgres"]["environment"]["GITHUB_TOKEN"], "ghp_test");
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
        mcp_servers.insert("custom".to_string(), OpenCodeMcpServerConfig {
            command: "python".to_string(),
            args: Some(vec!["server.py".to_string()]),
            env: Some(env),
        });
        config.mcp_servers = mcp_servers;

        let json = build_opencode_config_json(&config);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["mcp"]["custom"]["environment"]["API_KEY"], "secret123");
    }

    #[test]
    fn build_config_mcp_server_without_args() {
        let mut config = empty_config();

        let mut mcp_servers = HashMap::new();
        mcp_servers.insert("simple".to_string(), OpenCodeMcpServerConfig {
            command: "custom-binary".to_string(),
            args: None, // No args
            env: None,
        });
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
        assert_eq!(parsed["provider"]["myprovider"]["options"]["apiKey"], "sk-12345literal");
    }

    #[test]
    fn build_config_api_key_from_actual_env_var() {
        // Set a temporary env var for this test
        std::env::set_var("CORTEX_TEST_API_KEY_FOR_BUILD_CONFIG", "test-secret-key-12345");

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
        agents.insert("coder".to_string(), OpenCodeAgentConfig {
            model: Some("anthropic/claude-3".to_string()),
            instructions: Some("You are a coding assistant".to_string()),
            tools: Some(vec!["read".to_string(), "write".to_string(), "bash".to_string()]),
            max_turns: Some(20),
            disable: Some(false),
            permission: None,
        });
        config.agents = agents;

        let mut mcp_servers = HashMap::new();
        mcp_servers.insert("filesystem".to_string(), OpenCodeMcpServerConfig {
            command: "npx".to_string(),
            args: Some(vec!["-y".to_string(), "@anthropic/mcp-server".to_string()]),
            env: None,
        });
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
        assert_eq!(parsed["provider"]["z.ai"]["options"]["apiKey"], "sk-test-complex-key");

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
        assert!(parsed.get("model").is_none(), "model should be omitted when id is empty");
        assert!(parsed.get("agent").is_none(), "agent should be omitted when empty");
        assert!(parsed.get("mcp").is_none(), "mcp should be omitted when empty");
        assert!(parsed.get("provider").is_none(), "provider should be omitted when no api_key_env");

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
