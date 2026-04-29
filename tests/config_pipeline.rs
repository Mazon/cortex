//! Integration test: full config load → validate → finalize pipeline.
//!
//! Uses temp files to exercise the real file I/O path.
//! Includes proptest-based property tests for serialization round-trips.

use std::io::Write;

/// Helper: write a TOML string to a temp file and return its path.
fn write_temp_config(content: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    write!(file, "{}", content).expect("Failed to write temp config");
    file
}

#[test]
fn config_pipeline_minimal_valid_config() {
    let file = write_temp_config(
        r#"
        [opencode]
        port = 11643

        [[columns.definitions]]
        id = "todo"
        visible = true

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let config = cortex::config::load_config(file.path()).expect("load_config should succeed");
    assert_eq!(config.columns.definitions.len(), 1);
    assert_eq!(config.columns.definitions[0].id, "todo");
    assert_eq!(config.columns.visible_column_ids(), &["todo"]);
}

#[test]
fn config_pipeline_multi_column_with_auto_progress() {
    let file = write_temp_config(
        r#"
        [[columns.definitions]]
        id = "todo"
        visible = true

        [[columns.definitions]]
        id = "planning"
        visible = true
        agent = "planning"
        auto_progress_to = "running"

        [[columns.definitions]]
        id = "running"
        visible = true
        agent = "do"

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "debug"
        "#,
    );
    let config = cortex::config::load_config(file.path()).expect("load_config should succeed");
    assert_eq!(config.columns.definitions.len(), 3);
    assert_eq!(
        config.columns.visible_column_ids(),
        &["todo", "planning", "running"]
    );
    assert_eq!(
        config.columns.auto_progress_for("planning"),
        Some("running".to_string())
    );
}

#[test]
fn config_pipeline_hidden_columns_excluded_from_visible_ids() {
    let file = write_temp_config(
        r#"
        [[columns.definitions]]
        id = "todo"
        visible = true

        [[columns.definitions]]
        id = "backlog"
        visible = false

        [[columns.definitions]]
        id = "done"
        visible = false

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let config = cortex::config::load_config(file.path()).expect("load_config should succeed");
    assert_eq!(config.columns.visible_column_ids(), &["todo"]);
}

#[test]
fn config_pipeline_empty_columns_rejected() {
    let file = write_temp_config(
        r#"
        [columns]

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let result = cortex::config::load_config(file.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("must not be empty"), "got: {}", msg);
}

#[test]
fn config_pipeline_duplicate_column_ids_rejected() {
    let file = write_temp_config(
        r#"
        [[columns.definitions]]
        id = "todo"
        visible = true

        [[columns.definitions]]
        id = "todo"
        visible = true

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let result = cortex::config::load_config(file.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("Duplicate column ID"), "got: {}", msg);
}

#[test]
fn config_pipeline_auto_progress_to_nonexistent_rejected() {
    let file = write_temp_config(
        r#"
        [[columns.definitions]]
        id = "todo"
        visible = true
        auto_progress_to = "nonexistent"

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let result = cortex::config::load_config(file.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("non-existent column"), "got: {}", msg);
}

#[test]
fn config_pipeline_missing_file_generates_default() {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("cortex.toml");
    assert!(!path.exists());

    let config = cortex::config::load_config(&path).expect("Should create default config");
    // Default config has 5 columns
    assert_eq!(config.columns.definitions.len(), 5);
    assert_eq!(config.columns.definitions[0].id, "todo");
    // The file should now exist
    assert!(path.exists());
}

#[test]
fn config_pipeline_zero_port_rejected() {
    let file = write_temp_config(
        r#"
        [opencode]
        port = 0

        [[columns.definitions]]
        id = "todo"
        visible = true

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let result = cortex::config::load_config(file.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("port must be > 0"), "got: {}", msg);
}

#[test]
fn config_pipeline_invalid_toml_rejected() {
    let file = write_temp_config("this is not valid toml [[[");
    let result = cortex::config::load_config(file.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("Failed to parse"), "got: {}", msg);
}

#[test]
fn config_pipeline_log_level_validation() {
    let file = write_temp_config(
        r#"
        [[columns.definitions]]
        id = "todo"
        visible = true

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "invalid_level"
        "#,
    );
    let result = cortex::config::load_config(file.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("log.level must be one of"), "got: {}", msg);
}

#[test]
fn config_pipeline_custom_agents_with_column_validation() {
    let file = write_temp_config(
        r#"
        [opencode.agents.planner]
        instructions = "Plan the task"

        [opencode.agents.doer]
        instructions = "Execute the task"

        [[columns.definitions]]
        id = "todo"
        visible = true

        [[columns.definitions]]
        id = "planning"
        visible = true
        agent = "planner"

        [[columns.definitions]]
        id = "running"
        visible = true
        agent = "doer"

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let config = cortex::config::load_config(file.path()).expect("Should succeed");
    assert_eq!(config.opencode.agents.len(), 2);
    assert_eq!(
        config.columns.agent_for_column("planning"),
        Some("planner".to_string())
    );
}

#[test]
fn config_pipeline_agent_mismatch_warns_but_succeeds() {
    // Agent mismatch is a soft warning, not a hard error.
    // The agents section provides optional per-agent overrides; the opencode
    // server is the authority on which agents actually exist.
    let file = write_temp_config(
        r#"
        [opencode.agents.planner]
        instructions = "Plan the task"

        [[columns.definitions]]
        id = "todo"
        visible = true
        agent = "nonexistent_agent"

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let result = cortex::config::load_config(file.path());
    assert!(result.is_ok(), "agent mismatch should only warn, not error");
    let config = result.unwrap();
    assert_eq!(
        config.columns.agent_for_column("todo"),
        Some("nonexistent_agent".to_string())
    );
}

#[test]
fn config_pipeline_cycle_detected_rejected() {
    let file = write_temp_config(
        r#"
        [[columns.definitions]]
        id = "a"
        visible = true
        auto_progress_to = "b"

        [[columns.definitions]]
        id = "b"
        visible = true
        auto_progress_to = "a"

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let result = cortex::config::load_config(file.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("Cycle detected"), "got: {}", msg);
}

#[test]
fn config_pipeline_valid_linear_chain_passes() {
    let file = write_temp_config(
        r#"
        [[columns.definitions]]
        id = "todo"
        visible = true
        auto_progress_to = "planning"

        [[columns.definitions]]
        id = "planning"
        visible = true
        auto_progress_to = "running"

        [[columns.definitions]]
        id = "running"
        visible = true
        auto_progress_to = "done"

        [[columns.definitions]]
        id = "done"
        visible = false

        [theme]
        sidebar_width = 20
        column_width = 30

        [log]
        level = "info"
        "#,
    );
    let config = cortex::config::load_config(file.path()).expect("Linear chain should be valid");
    assert_eq!(config.columns.definitions.len(), 4);
    assert_eq!(
        config.columns.visible_column_ids(),
        &["todo", "planning", "running"]
    );
}

// ─── Proptest: config serialization round-trip ─────────────────────────

use cortex::config::types::{
    ColumnConfig, ColumnsConfig, CortexConfig, KeybindingConfig, LogConfig, OpenCodeConfig,
    ThemeConfig,
};
use proptest::prelude::*;

/// Generate a valid column ID string (1–32 chars, alphanumeric + hyphens).
fn arb_column_id() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{0,31}"
}

/// Generate a valid log level string.
fn arb_log_level() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("trace".to_string()),
        Just("debug".to_string()),
        Just("info".to_string()),
        Just("warn".to_string()),
        Just("error".to_string()),
    ]
}

/// Generate a valid port (1–65535).
fn arb_port() -> impl Strategy<Value = u16> {
    1u16..=65535u16
}

/// Generate a valid sidebar/column width (1–200).
fn arb_width() -> impl Strategy<Value = u16> {
    1u16..=200u16
}

/// Strategy for a valid `LogConfig`.
fn arb_log_config() -> impl Strategy<Value = LogConfig> {
    arb_log_level().prop_map(|level| LogConfig { level })
}

/// Strategy for a valid `ColumnConfig`.
fn arb_column_config() -> impl Strategy<Value = ColumnConfig> {
    (
        arb_column_id(),
        proptest::option::of("[a-zA-Z ]{1,64}"),
        proptest::option::of("[a-z][a-z0-9_-]{0,31}"),
        proptest::option::of(Just(true)), // visible: always true for valid config
    ).prop_map(|(id, display_name, agent, _visible)| ColumnConfig {
        id,
        display_name,
        visible: true,
        agent,
        auto_progress_to: None, // Don't generate auto_progress to avoid cycle detection
    })
}

/// Strategy for a valid `ColumnsConfig` with 1–5 columns.
fn arb_columns_config() -> impl Strategy<Value = ColumnsConfig> {
    prop::collection::vec(arb_column_config(), 1..=5).prop_map(|definitions| {
        let mut config = CortexConfig::default();
        config.columns.definitions = definitions;
        config.columns.finalize();
        // Extract just the columns config
        std::mem::take(&mut config.columns)
    })
}

/// Strategy for a valid `OpenCodeConfig`.
fn arb_opencode_config() -> impl Strategy<Value = OpenCodeConfig> {
    arb_port().prop_map(|port| OpenCodeConfig {
        port,
        ..OpenCodeConfig::default()
    })
}

/// Strategy for a valid `ThemeConfig`.
fn arb_theme_config() -> impl Strategy<Value = ThemeConfig> {
    (arb_width(), arb_width()).prop_map(|(sidebar_width, column_width)| ThemeConfig {
        sidebar_width,
        column_width,
        ..ThemeConfig::default()
    })
}

/// Strategy for a valid `CortexConfig`.
fn arb_cortex_config() -> impl Strategy<Value = CortexConfig> {
    (
        arb_opencode_config(),
        arb_columns_config(),
        arb_theme_config(),
        arb_log_config(),
    ).prop_map(|(opencode, columns, theme, log)| CortexConfig {
        opencode,
        columns,
        keybindings: KeybindingConfig::default(),
        theme,
        log,
    })
}

proptest! {
    /// Property: serializing a CortexConfig to TOML and deserializing
    /// it back produces an equivalent config.
    #[test]
    fn prop_config_roundtrip(config in arb_cortex_config()) {
        let toml_str = toml::to_string(&config).expect("serialize failed");
        let deserialized: CortexConfig =
            toml::from_str(&toml_str).expect("deserialize failed");

        // Compare fields that survive round-trip
        assert_eq!(config.opencode.port, deserialized.opencode.port);
        assert_eq!(config.log.level, deserialized.log.level);
        assert_eq!(config.theme.sidebar_width, deserialized.theme.sidebar_width);
        assert_eq!(config.theme.column_width, deserialized.theme.column_width);
        assert_eq!(config.columns.definitions.len(), deserialized.columns.definitions.len());

        for (orig, de) in config.columns.definitions.iter().zip(deserialized.columns.definitions.iter()) {
            assert_eq!(orig.id, de.id);
            assert_eq!(orig.display_name, de.display_name);
            assert_eq!(orig.visible, de.visible);
            assert_eq!(orig.agent, de.agent);
            assert_eq!(orig.auto_progress_to, de.auto_progress_to);
        }
    }

    /// Property: `toml::to_string` never panics for any valid CortexConfig.
    #[test]
    fn prop_config_serialize_never_panics(config in arb_cortex_config()) {
        let _ = toml::to_string(&config);
    }

    /// Property: `toml::from_str` never panics for any valid TOML output.
    #[test]
    fn prop_config_deserialize_never_panics(config in arb_cortex_config()) {
        let toml_str = toml::to_string(&config).expect("serialize failed");
        let _ = toml::from_str::<CortexConfig>(&toml_str);
    }

    /// Property: config with various log levels round-trips correctly.
    #[test]
    fn prop_log_level_roundtrip(level in arb_log_level()) {
        let config = LogConfig { level };
        let toml_str = toml::to_string(&config).expect("serialize failed");
        let deserialized: LogConfig =
            toml::from_str(&toml_str).expect("deserialize failed");
        assert_eq!(config.level, deserialized.level);
    }

    /// Property: columns config round-trips with correct visible IDs.
    #[test]
    fn prop_columns_visible_ids_roundtrip(columns in arb_columns_config()) {
        let toml_str = toml::to_string(&columns).expect("serialize failed");
        let mut deserialized: ColumnsConfig =
            toml::from_str(&toml_str).expect("deserialize failed");
        deserialized.finalize();

        assert_eq!(
            columns.visible_column_ids(),
            deserialized.visible_column_ids(),
            "visible IDs should survive round-trip"
        );
    }
}
