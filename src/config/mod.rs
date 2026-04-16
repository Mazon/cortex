//! Configuration loading, saving, and validation.

pub mod defaults;
pub mod types;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use defaults::default_config;
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
/// `$HOME/.config` when it is not set. As a last resort, returns `/tmp`.
pub fn xdg_config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(|_| PathBuf::from("/tmp"))
        })
}

/// Returns the XDG data home directory.
///
/// Respects the `XDG_DATA_HOME` environment variable, falling back to
/// `$HOME/.local/share` when it is not set. As a last resort, returns `/tmp`.
pub fn xdg_data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".local").join("share"))
                .unwrap_or_else(|_| PathBuf::from("/tmp"))
        })
}

/// Load config from a TOML file. If the file doesn't exist, return defaults.
/// If the file exists, parse it and deep-merge with defaults for any missing fields.
pub fn load_config(path: &Path) -> Result<CortexConfig> {
    if !path.exists() {
        tracing::info!("Config file not found at {:?}, using defaults", path);
        return Ok(default_config());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {:?}", path))?;

    let user_config: CortexConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {:?}", path))?;

    // Validate
    validate_config(&user_config)?;

    tracing::info!("Loaded config from {:?}", path);
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

    tracing::info!("Saved config to {:?}", path);
    Ok(())
}

/// Basic config validation.
fn validate_config(config: &CortexConfig) -> Result<()> {
    // Validate that column IDs are unique
    let mut seen = std::collections::HashSet::new();
    for col in &config.columns.definitions {
        if !seen.insert(&col.id) {
            anyhow::bail!("Duplicate column ID: {}", col.id);
        }
    }

    // Validate auto_progress_to targets exist
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

    // Validate port range
    if config.opencode.port == 0 {
        anyhow::bail!("opencode.port must be > 0");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = default_config();
        assert_eq!(config.columns.definitions.len(), 5);
        assert_eq!(config.columns.definitions[0].id, "todo");
        assert_eq!(config.opencode.port, 11643);
    }

    #[test]
    fn test_default_config_toml_parse() {
        let config: CortexConfig = toml::from_str(defaults::DEFAULT_CONFIG_TOML).unwrap();
        assert_eq!(config.columns.definitions.len(), 5);
    }

    #[test]
    fn test_validate_duplicate_column() {
        let mut config = default_config();
        let dup = config.columns.definitions[0].clone();
        config.columns.definitions.push(dup);
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_validate_bad_auto_progress() {
        let mut config = default_config();
        config.columns.definitions[0].auto_progress_to = Some("nonexistent".to_string());
        assert!(validate_config(&config).is_err());
    }
}
