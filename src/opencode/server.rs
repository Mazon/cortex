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

impl Default for OpenCodeServer {
    fn default() -> Self {
        Self::new().expect("Failed to create default OpenCodeServer")
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
