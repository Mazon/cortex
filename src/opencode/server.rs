//! OpenCode server manager — single shared server for all projects.

use anyhow::{Context, Result};
use tokio::process::{Child, Command};
use tokio::time::Duration;

use crate::config::serialization::build_opencode_config_json;
use crate::config::types::OpenCodeConfig;

const INITIAL_WAIT: Duration = Duration::from_secs(2);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_START_RETRIES: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Manages the single shared OpenCode server process for all projects.
///
/// Instead of spawning one `opencode serve` per project, this manager
/// maintains a single server instance. Sessions are differentiated by
/// the OpenCode server's internal project scoping.
pub struct ServerManager {
    server: Option<OpenCodeServer>,
    /// The URL the shared server is listening on (once started).
    url: Option<String>,
}

impl ServerManager {
    pub fn new() -> Self {
        Self {
            server: None,
            url: None,
        }
    }

    /// Start the shared server (if not already running) and return its URL.
    ///
    /// `working_dir` is the first project's working directory — the server
    /// uses it as its initial cwd. The server itself handles multi-project
    /// session scoping internally.
    pub async fn start_shared(
        &mut self,
        config: &OpenCodeConfig,
        working_dir: &str,
    ) -> Result<String> {
        // If already running, return the cached URL
        if let Some(ref url) = self.url {
            let running = self.server.as_mut().map(|s| s.is_running()).unwrap_or(false);
            if running {
                return Ok(url.clone());
            }
        }

        let mut server = OpenCodeServer::new()?;
        server.start(config, working_dir).await?;
        let url = server.url().to_string();

        self.url = Some(url.clone());
        self.server = Some(server);
        Ok(url)
    }

    /// Stop the shared server.
    pub async fn stop_all(&mut self) {
        if let Some(mut server) = self.server.take() {
            let _ = server.stop().await;
        }
        self.url = None;
    }
}

/// Manages a single OpenCode server process.
struct OpenCodeServer {
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

        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 0..=MAX_START_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(RETRY_DELAY).await;
            }

            match self.spawn_server(host, port, &server_config_json, working_dir).await {
                Ok(()) => match self.wait_for_healthy().await {
                    Ok(()) => {
                        return Ok(());
                    }
                    Err(e) => {
                        last_err = Some(e);
                        self.kill_process().await;
                    }
                },
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }

        let msg = match &last_err {
            Some(e) => format!(
                "Server failed to start after {} attempts: {}",
                MAX_START_RETRIES + 1,
                e
            ),
            None => format!(
                "Server failed to start after {} attempts",
                MAX_START_RETRIES + 1
            ),
        };
        anyhow::bail!("{}", msg);
    }

    async fn spawn_server(
        &mut self,
        host: &str,
        port: u16,
        config_json: &str,
        working_dir: &str,
    ) -> Result<()> {
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
                    return Ok(());
                }
                Ok(_resp) => {
                }
                Err(_e) => {
                }
            }

            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    /// Stop the server process.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.process.take() {
            match child.start_kill() {
                Ok(()) => {}
                Err(e) => {
                    return Err(e).context("Failed to stop process");
                }
            }
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(_status)) => {}
                Ok(Err(e)) => { let _ = e; }
                Err(_) => {}
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
