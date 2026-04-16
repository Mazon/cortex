//! Default configuration values.

use super::types::*;

/// Returns a sensible default config.
pub fn default_config() -> CortexConfig {
    CortexConfig {
        opencode: OpenCodeConfig::default(),
        columns: ColumnsConfig::default(),
        keybindings: KeybindingConfig::default(),
        theme: ThemeConfig::default(),
        log: LogConfig::default(),
    }
}
