//! Configuration loading, saving, and validation.

pub mod serialization;
pub mod types;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use types::CortexConfig;

/// Returns the default config path: `$XDG_CONFIG_HOME/cortex/cortex.toml`.
///
/// Respects the `XDG_CONFIG_HOME` environment variable, falling back to
/// `$HOME/.config` when it is not set.
pub fn default_config_path() -> PathBuf {
    xdg_config_home().join("cortex").join("cortex.toml")
}

/// Returns the XDG config home directory.
///
/// Respects the `XDG_CONFIG_HOME` environment variable, falling back to
/// `$HOME/.config` when it is not set. As a last resort, returns `/tmp`
/// and emits a warning since data stored there is world-readable and
/// cleared on reboot.
pub fn xdg_config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(|_| {
                    tracing::warn!(
                        "Neither $XDG_CONFIG_HOME nor $HOME is set. \
                         Falling back to /tmp for config directory — \
                         data will be lost on reboot"
                    );
                    PathBuf::from("/tmp")
                })
        })
}

/// Returns the XDG data home directory.
///
/// Respects the `XDG_DATA_HOME` environment variable, falling back to
/// `$HOME/.local/share` when it is not set. As a last resort, returns `/tmp`
/// and emits a warning since data stored there is world-readable and
/// cleared on reboot.
pub fn xdg_data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".local").join("share"))
                .unwrap_or_else(|_| {
                    tracing::warn!(
                        "Neither $XDG_DATA_HOME nor $HOME is set. \
                         Falling back to /tmp for data directory — \
                         database and logs will be lost on reboot"
                    );
                    PathBuf::from("/tmp")
                })
        })
}

/// Load config from a TOML file. If the file doesn't exist, generate a default
/// config file at the path and return the defaults. This lets users discover and
/// customize settings without needing to consult documentation.
/// If the file exists, parse it and apply serde defaults for any missing fields.
/// Column definitions are replaced entirely, not merged.
///
/// # Example
///
/// ```no_run
/// use cortex::config::load_config;
///
/// let config = load_config(&default_path)?;
/// println!("OpenCode port: {}", config.opencode.port);
/// ```
pub fn load_config(path: &Path) -> Result<CortexConfig> {
    if !path.exists() {
        let config = CortexConfig::default();
        save_config(&config, path)
            .with_context(|| format!("Failed to generate default config at {:?}", path))?;
        return Ok(config);
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {:?}", path))?;

    let mut user_config: CortexConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {:?}", path))?;

    // Validate
    validate_config(&user_config)?;

    // Populate derived caches (e.g. visible column IDs)
    user_config.columns.finalize();

    Ok(user_config)
}

/// Save config to a TOML file. Creates parent directories if needed.
pub fn save_config(config: &CortexConfig, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {:?}", parent))?;
    }

    let content = toml::to_string_pretty(config).context("Failed to serialize config to TOML")?;

    std::fs::write(path, content)
        .with_context(|| format!("Failed to write config file: {:?}", path))?;

    Ok(())
}

/// Maximum allowed length for column IDs. Very long IDs can cause rendering issues
/// in the TUI and are almost certainly a user mistake.
const MAX_COLUMN_ID_LENGTH: usize = 64;

/// Basic config validation.
fn validate_config(config: &CortexConfig) -> Result<()> {
    // Validate that column definitions are not empty
    if config.columns.definitions.is_empty() {
        anyhow::bail!("columns.definitions must not be empty");
    }

    // Validate that column IDs are unique and not excessively long
    let mut seen = std::collections::HashSet::new();
    for col in &config.columns.definitions {
        if col.id.trim().is_empty() {
            anyhow::bail!("Column ID must not be empty");
        }
        if col.id.len() > MAX_COLUMN_ID_LENGTH {
            anyhow::bail!(
                "Column ID '{}' exceeds maximum length of {} characters",
                col.id,
                MAX_COLUMN_ID_LENGTH
            );
        }
        if !seen.insert(&col.id) {
            anyhow::bail!("Duplicate column ID: {}", col.id);
        }
    }

    // Validate auto_progress_to targets exist and detect cycles
    for col in &config.columns.definitions {
        if let Some(ref target) = col.auto_progress_to {
            let exists = config.columns.definitions.iter().any(|c| c.id == *target);
            if !exists {
                anyhow::bail!(
                    "Column '{}' auto_progress_to '{}' targets a non-existent column",
                    col.id,
                    target
                );
            }
        }
    }

    // Detect cycles in auto_progress_to chain (could cause infinite loops).
    // Uses Floyd's tortoise-and-hare algorithm for O(n) cycle detection.
    for col in &config.columns.definitions {
        if let Some(ref start) = col.auto_progress_to {
            let mut visited = std::collections::HashSet::new();
            let mut current = start.as_str();
            loop {
                if !visited.insert(current) {
                    // We've seen this column before — cycle detected
                    anyhow::bail!(
                        "Cycle detected in auto_progress chain starting at column '{}': {} → ... → {}",
                        col.id,
                        col.id,
                        current
                    );
                }
                // Find the column and follow its auto_progress_to
                match config.columns.definitions.iter().find(|c| c.id == current) {
                    Some(next_col) => {
                        match &next_col.auto_progress_to {
                            Some(next_target) => current = next_target,
                            None => break, // chain terminates, no cycle from this start
                        }
                    }
                    None => break, // shouldn't happen after the existence check above
                }
            }
        }
    }

    // Validate port range (port is u16, so max value is 65535; reject 0 as invalid)
    if config.opencode.port == 0 {
        anyhow::bail!("opencode.port must be > 0");
    }

    // Validate layout dimensions — zero widths cause rendering failures
    if config.theme.sidebar_width == 0 {
        anyhow::bail!("theme.sidebar_width must be > 0");
    }
    if config.theme.column_width == 0 {
        anyhow::bail!("theme.column_width must be > 0");
    }

    // Validate log level is a recognized tracing level
    const VALID_LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];
    if !VALID_LOG_LEVELS.contains(&config.log.level.to_lowercase().as_str()) {
        anyhow::bail!(
            "log.level must be one of: {} (got '{}')",
            VALID_LOG_LEVELS.join(", "),
            config.log.level
        );
    }

    // Warn when column agent references lack config overrides.
    // The agents section provides optional per-agent overrides; the opencode
    // server is the authority on which agents actually exist. This is a soft
    // warning, not a hard error, since agents may be discovered at runtime.
    for col in &config.columns.definitions {
        if let Some(ref agent_name) = col.agent {
            if !config.opencode.agents.is_empty()
                && !config.opencode.agents.contains_key(agent_name)
            {
                tracing::warn!(
                    "Column '{}' references agent '{}' which has no config \
                     override in opencode.agents — the opencode server will \
                     resolve it at runtime",
                    col.id,
                    agent_name
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{ColumnConfig, CortexConfig, OpenCodeConfig};

    /// Helper to build a minimal valid config with one column.
    fn minimal_config() -> CortexConfig {
        CortexConfig {
            opencode: OpenCodeConfig::default(),
            columns: types::ColumnsConfig {
                definitions: vec![ColumnConfig {
                    id: "todo".to_string(),
                    display_name: None,
                    visible: true,
                    agent: None,
                    auto_progress_to: None,
                }],
                visible_ids: vec!["todo".to_string()],
            },
            keybindings: types::KeybindingConfig::default(),
            theme: types::ThemeConfig::default(),
            log: types::LogConfig::default(),
        }
    }

    #[test]
    fn test_default_config() {
        let config = CortexConfig::default();
        assert_eq!(config.columns.definitions.len(), 5);
        assert_eq!(config.columns.definitions[0].id, "todo");
        assert_eq!(config.opencode.port, 11643);
    }

    #[test]
    fn test_validate_duplicate_column() {
        let mut config = CortexConfig::default();
        let dup = config.columns.definitions[0].clone();
        config.columns.definitions.push(dup);
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Duplicate column ID: todo"), "got: {}", msg);
    }

    #[test]
    fn test_validate_bad_auto_progress() {
        let mut config = CortexConfig::default();
        config.columns.definitions[0].auto_progress_to = Some("nonexistent".to_string());
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("non-existent column"), "got: {}", msg);
    }

    // ─── Empty column definitions ───

    #[test]
    fn test_validate_empty_column_definitions() {
        let mut config = minimal_config();
        config.columns.definitions.clear();
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("must not be empty"), "got: {}", msg);
    }

    // ─── Empty column ID ───

    #[test]
    fn test_validate_empty_column_id() {
        let mut config = minimal_config();
        config.columns.definitions[0].id = String::new();
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("must not be empty"), "got: {}", msg);
    }

    // ─── Very long column IDs ───

    #[test]
    fn test_validate_very_long_column_id() {
        let mut config = minimal_config();
        config.columns.definitions[0].id = "a".repeat(65);
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("exceeds maximum length"), "got: {}", msg);
    }

    #[test]
    fn test_validate_column_id_at_max_length() {
        let mut config = minimal_config();
        config.columns.definitions[0].id = "a".repeat(MAX_COLUMN_ID_LENGTH);
        // Exactly at the boundary should pass
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_validate_column_id_just_over_max_length() {
        let mut config = minimal_config();
        config.columns.definitions[0].id = "a".repeat(MAX_COLUMN_ID_LENGTH + 1);
        let result = validate_config(&config);
        assert!(result.is_err());
    }

    // ─── Port validation ───

    #[test]
    fn test_validate_port_zero() {
        let mut config = minimal_config();
        config.opencode.port = 0;
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("port must be > 0"), "got: {}", msg);
    }

    #[test]
    fn test_validate_port_max_value() {
        let mut config = minimal_config();
        config.opencode.port = 65535;
        // Max u16 port should be valid
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_validate_port_one() {
        let mut config = minimal_config();
        config.opencode.port = 1;
        // Port 1 is the minimum valid port
        assert!(validate_config(&config).is_ok());
    }

    // ─── Duplicate column IDs (extended) ───

    #[test]
    fn test_validate_multiple_duplicate_columns() {
        let mut config = minimal_config();
        config.columns.definitions.push(ColumnConfig {
            id: "todo".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: None,
        });
        config.columns.definitions.push(ColumnConfig {
            id: "todo".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: None,
        });
        let result = validate_config(&config);
        assert!(result.is_err());
        // Should report the first duplicate found
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Duplicate column ID"), "got: {}", msg);
    }

    // ─── Auto-progress validation (extended) ───

    #[test]
    fn test_validate_auto_progress_self_reference_is_cycle() {
        let mut config = minimal_config();
        config.columns.definitions[0].auto_progress_to = Some("todo".to_string());
        // Self-reference creates a cycle (a → a), so it should be rejected
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Cycle detected"), "got: {}", msg);
    }

    #[test]
    fn test_validate_auto_progress_to_valid_column() {
        let mut config = minimal_config();
        config.columns.definitions.push(ColumnConfig {
            id: "done".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: None,
        });
        config.columns.definitions[0].auto_progress_to = Some("done".to_string());
        assert!(validate_config(&config).is_ok());
    }

    // ─── Valid configs pass ───

    #[test]
    fn test_validate_default_config_passes() {
        let config = CortexConfig::default();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_validate_minimal_config_passes() {
        let config = minimal_config();
        assert!(validate_config(&config).is_ok());
    }

    // ─── Column ID edge cases ───

    #[test]
    fn test_validate_column_id_with_whitespace() {
        let mut config = minimal_config();
        config.columns.definitions[0].id = "  ".to_string();
        // Whitespace-only IDs should be rejected
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("must not be empty"), "got: {}", msg);
    }

    #[test]
    fn test_validate_column_id_with_special_characters() {
        let mut config = minimal_config();
        config.columns.definitions[0].id = "my-column_123".to_string();
        // IDs with hyphens and underscores should be valid
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_validate_empty_display_name_allowed() {
        let mut config = minimal_config();
        config.columns.definitions[0].display_name = Some(String::new());
        // Empty display_name should be fine — it's cosmetic
        assert!(validate_config(&config).is_ok());
    }

    // ─── sidebar_width / column_width validation (F-32) ───

    #[test]
    fn test_validate_sidebar_width_zero_rejected() {
        let mut config = minimal_config();
        config.theme.sidebar_width = 0;
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("sidebar_width must be > 0"), "got: {}", msg);
    }

    #[test]
    fn test_validate_column_width_zero_rejected() {
        let mut config = minimal_config();
        config.theme.column_width = 0;
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("column_width must be > 0"), "got: {}", msg);
    }

    #[test]
    fn test_validate_sidebar_width_one_is_valid() {
        let mut config = minimal_config();
        config.theme.sidebar_width = 1;
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_validate_column_width_one_is_valid() {
        let mut config = minimal_config();
        config.theme.column_width = 1;
        assert!(validate_config(&config).is_ok());
    }

    // ─── log.level validation (F-32) ───

    #[test]
    fn test_validate_log_level_valid() {
        for level in &["trace", "debug", "info", "warn", "error"] {
            let mut config = minimal_config();
            config.log.level = level.to_string();
            assert!(
                validate_config(&config).is_ok(),
                "level '{}' should be valid",
                level
            );
        }
    }

    #[test]
    fn test_validate_log_level_case_insensitive() {
        let mut config = minimal_config();
        config.log.level = "INFO".to_string();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_validate_log_level_invalid() {
        let mut config = minimal_config();
        config.log.level = "verbose".to_string();
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("log.level must be one of"), "got: {}", msg);
        assert!(msg.contains("verbose"), "got: {}", msg);
    }

    #[test]
    fn test_validate_log_level_empty() {
        let mut config = minimal_config();
        config.log.level = String::new();
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("log.level must be one of"), "got: {}", msg);
    }

    // ─── Auto-progress cycle detection ───────────────────────────────────

    #[test]
    fn test_validate_auto_progress_self_cycle_detected() {
        let mut config = minimal_config();
        config.columns.definitions[0].auto_progress_to = Some("todo".to_string());
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Cycle detected"), "got: {}", msg);
    }

    #[test]
    fn test_validate_auto_progress_two_node_cycle() {
        let mut config = minimal_config();
        config.columns.definitions.push(ColumnConfig {
            id: "step2".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: Some("todo".to_string()),
        });
        config.columns.definitions[0].auto_progress_to = Some("step2".to_string());
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Cycle detected"), "got: {}", msg);
    }

    #[test]
    fn test_validate_auto_progress_three_node_cycle() {
        let mut config = minimal_config();
        config.columns.definitions[0].auto_progress_to = Some("b".to_string());
        config.columns.definitions.push(ColumnConfig {
            id: "b".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: Some("c".to_string()),
        });
        config.columns.definitions.push(ColumnConfig {
            id: "c".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: Some("todo".to_string()),
        });
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Cycle detected"), "got: {}", msg);
    }

    #[test]
    fn test_validate_auto_progress_chain_no_cycle() {
        let mut config = minimal_config();
        config.columns.definitions[0].auto_progress_to = Some("b".to_string());
        config.columns.definitions.push(ColumnConfig {
            id: "b".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: Some("c".to_string()),
        });
        config.columns.definitions.push(ColumnConfig {
            id: "c".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: None, // terminates here
        });
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_validate_auto_progress_multiple_chains_no_cycle() {
        let mut config = minimal_config();
        config.columns.definitions[0].auto_progress_to = Some("done".to_string());
        config.columns.definitions.push(ColumnConfig {
            id: "b".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: Some("done".to_string()),
        });
        config.columns.definitions.push(ColumnConfig {
            id: "done".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: None,
        });
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_validate_auto_progress_cycle_with_branch() {
        // a → b, a → c (a has one auto_progress, c points back to a)
        // Actually auto_progress_to is a single target, so:
        // a → b → c → a (cycle)
        let mut config = minimal_config();
        config.columns.definitions[0].auto_progress_to = Some("b".to_string());
        config.columns.definitions.push(ColumnConfig {
            id: "b".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: Some("c".to_string()),
        });
        config.columns.definitions.push(ColumnConfig {
            id: "c".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: Some("d".to_string()),
        });
        config.columns.definitions.push(ColumnConfig {
            id: "d".to_string(),
            display_name: None,
            visible: true,
            agent: None,
            auto_progress_to: Some("b".to_string()), // cycle: b → c → d → b
        });
        let result = validate_config(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Cycle detected"), "got: {}", msg);
    }

    /// Property-based test: generate arbitrary-length chains and verify
    /// that cycles of any length are detected.
    #[test]
    fn test_validate_auto_progress_property_arbitrary_cycle_lengths() {
        // Test cycles of length 1 through 10
        for cycle_len in 1..=10usize {
            let mut definitions = Vec::new();
            let names: Vec<String> = (0..cycle_len).map(|i| format!("col{}", i)).collect();

            // Create columns with a cycle: col0 → col1 → ... → col(n-1) → col0
            for i in 0..cycle_len {
                let target = if i + 1 < cycle_len {
                    names[i + 1].clone()
                } else {
                    names[0].clone() // back to start
                };
                definitions.push(ColumnConfig {
                    id: names[i].clone(),
                    display_name: None,
                    visible: true,
                    agent: None,
                    auto_progress_to: Some(target),
                });
            }

            let config = CortexConfig {
                opencode: OpenCodeConfig::default(),
                columns: types::ColumnsConfig {
                    definitions,
                    visible_ids: Vec::new(),
                },
                keybindings: types::KeybindingConfig::default(),
                theme: types::ThemeConfig::default(),
                log: types::LogConfig::default(),
            };

            let result = validate_config(&config);
            assert!(
                result.is_err(),
                "Expected cycle detection for cycle length {}, got Ok",
                cycle_len
            );
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("Cycle detected"),
                "Expected 'Cycle detected' for length {}, got: {}",
                cycle_len,
                msg
            );
        }
    }

    /// Property-based test: generate linear chains of arbitrary length
    /// and verify they all pass validation (no false positive cycle detection).
    #[test]
    fn test_validate_auto_progress_property_linear_chains_pass() {
        // Test linear chains of length 1 through 20
        for chain_len in 1..=20usize {
            let mut definitions = Vec::new();
            let names: Vec<String> = (0..chain_len).map(|i| format!("col{}", i)).collect();

            // Create columns: col0 → col1 → ... → col(n-1) → (none)
            for i in 0..chain_len {
                let target = if i + 1 < chain_len {
                    Some(names[i + 1].clone())
                } else {
                    None // terminates
                };
                definitions.push(ColumnConfig {
                    id: names[i].clone(),
                    display_name: None,
                    visible: true,
                    agent: None,
                    auto_progress_to: target,
                });
            }

            let config = CortexConfig {
                opencode: OpenCodeConfig::default(),
                columns: types::ColumnsConfig {
                    definitions,
                    visible_ids: Vec::new(),
                },
                keybindings: types::KeybindingConfig::default(),
                theme: types::ThemeConfig::default(),
                log: types::LogConfig::default(),
            };

            let result = validate_config(&config);
            assert!(
                result.is_ok(),
                "Linear chain of length {} should be valid, got: {}",
                chain_len,
                result.unwrap_err()
            );
        }
    }

    /// Property-based test: generate chains where the cycle is not at the start.
    #[test]
    fn test_validate_auto_progress_property_cycle_in_middle() {
        // a → b → c → d → b (cycle starts at b, but a is the entry)
        for cycle_start in 0..5 {
            let names: Vec<String> = (0..7).map(|i| format!("col{}", i)).collect();

            let mut definitions = Vec::new();
            // col0 → col1 → ... → col6 (linear)
            for i in 0..7 {
                let target = if i + 1 < 7 {
                    Some(names[i + 1].clone())
                } else {
                    Some(names[cycle_start].clone()) // cycle back
                };
                definitions.push(ColumnConfig {
                    id: names[i].clone(),
                    display_name: None,
                    visible: true,
                    agent: None,
                    auto_progress_to: target,
                });
            }

            let config = CortexConfig {
                opencode: OpenCodeConfig::default(),
                columns: types::ColumnsConfig {
                    definitions,
                    visible_ids: Vec::new(),
                },
                keybindings: types::KeybindingConfig::default(),
                theme: types::ThemeConfig::default(),
                log: types::LogConfig::default(),
            };

            let result = validate_config(&config);
            assert!(
                result.is_err(),
                "Expected cycle detection with cycle_start={}, got Ok",
                cycle_start
            );
        }
    }
}
