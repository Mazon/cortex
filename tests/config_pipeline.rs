//! Integration test: full config load → validate → finalize pipeline.
//!
//! Uses temp files to exercise the real file I/O path.

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
fn config_pipeline_agent_mismatch_warns_but_loads() {
    // Agent mismatch is now a soft warning, not a hard error.
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
    assert!(result.is_ok(), "agent mismatch should warn, not error");
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
    assert_eq!(config.columns.visible_column_ids(), &["todo", "planning", "running"]);
}
