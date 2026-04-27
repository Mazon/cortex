//! SQLite database operations for persistence.

use anyhow::Result;
use crate::state::types::{
    AgentStatus, CortexProject, CortexTask, KanbanColumn, ProjectStatus,
};
use rusqlite::{params, Connection, Transaction};
use std::collections::HashMap;
use std::path::Path;

/// Database wrapper for SQLite persistence.
pub struct Db {
    pub conn: Connection,
}

/// Current schema version. Increment this and add a new migration function
/// in `run_migrations()` when modifying the database schema.
#[allow(dead_code)]
const CURRENT_SCHEMA_VERSION: u32 = 1;

impl Db {
    /// Open a database connection and run migrations.
    pub fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(BASE_SCHEMA)?;
        run_migrations(&conn)?;

        Ok(Self { conn })
    }

    // ─── Task CRUD ─────────────────────────────────────────────────────

    pub fn save_task(&self, task: &CortexTask) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tasks (id, number, title, description, column_id, session_id, agent_type, agent_status, error_message, plan_output, pending_permission_count, pending_question_count, project_id, created_at, updated_at, entered_column_at, last_activity_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                task.id,
                task.number,
                task.title,
                task.description,
                task.column.0,
                task.session_id,
                task.agent_type.as_deref().unwrap_or("none"),
                task.agent_status.to_string(),
                task.error_message,
                task.plan_output,
                task.pending_permission_count,
                task.pending_question_count,
                task.project_id,
                task.created_at,
                task.updated_at,
                task.entered_column_at,
                task.last_activity_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_tasks(&self, project_id: &str) -> Result<Vec<CortexTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, number, title, description, column_id, session_id, agent_type, agent_status, error_message, plan_output, pending_permission_count, pending_question_count, project_id, created_at, updated_at, entered_column_at, last_activity_at FROM tasks WHERE project_id = ?1 ORDER BY number",
        )?;

        let tasks = stmt
            .query_map(params![project_id], |row| {
                Ok(CortexTask {
                    id: row.get(0)?,
                    number: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    column: KanbanColumn(row.get::<_, String>(4)?),
                    session_id: row.get(5)?,
                    agent_type: row.get::<_, Option<String>>(6)?.filter(|s| s != "none"),
                    agent_status: parse_agent_status(&row.get::<_, String>(7)?),
                    error_message: row.get(8)?,
                    plan_output: row.get(9)?,
                    pending_permission_count: row.get(10)?,
                    pending_question_count: row.get(11)?,
                    project_id: row.get(12)?,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    entered_column_at: row.get(15)?,
                    last_activity_at: row.get(16)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(tasks)
    }

    pub fn delete_task(&self, task_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM kanban_order WHERE task_id = ?1",
            params![task_id],
        )?;
        self.conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![task_id])?;
        Ok(())
    }

    // ─── Project CRUD ──────────────────────────────────────────────────

    pub fn save_project(&self, project: &CortexProject) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO projects (id, name, working_directory, status, position) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project.id,
                project.name,
                project.working_directory,
                project_status_to_str(&project.status),
                project.position,
            ],
        )?;
        Ok(())
    }

    pub fn load_projects(&self) -> Result<Vec<CortexProject>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, working_directory, status, position FROM projects ORDER BY position",
        )?;

        let projects = stmt
            .query_map([], |row| {
                Ok(CortexProject {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    working_directory: row.get(2)?,
                    status: parse_project_status(&row.get::<_, String>(3)?),
                    position: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(projects)
    }

    pub fn delete_project(&self, project_id: &str) -> Result<()> {
        // Delete tasks and kanban order for this project
        let tasks = self.load_tasks(project_id)?;
        for task in &tasks {
            self.delete_task(&task.id)?;
        }
        self.conn
            .execute("DELETE FROM projects WHERE id = ?1", params![project_id])?;
        Ok(())
    }

    // ─── Kanban Order ──────────────────────────────────────────────────

    pub fn save_kanban_order(&self, column: &KanbanColumn, task_ids: &[String]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM kanban_order WHERE column_id = ?1",
            params![column.0],
        )?;
        for (pos, task_id) in task_ids.iter().enumerate() {
            self.conn.execute(
                "INSERT INTO kanban_order (column_id, task_id, position) VALUES (?1, ?2, ?3)",
                params![column.0, task_id, pos],
            )?;
        }
        Ok(())
    }

    pub fn load_kanban_order(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT column_id, task_id FROM kanban_order ORDER BY column_id, position")?;

        let mut order: HashMap<String, Vec<String>> = HashMap::new();
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let (col_id, task_id) = row?;
            order.entry(col_id).or_default().push(task_id);
        }

        Ok(order)
    }

    // ─── Metadata ──────────────────────────────────────────────────────

    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM metadata WHERE key = ?1")?;
        let result = stmt.query_row(params![key], |row| row.get::<_, String>(0));
        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_next_task_number(&self, project_id: &str) -> Result<u32> {
        let max: Option<u32> = self.conn.query_row(
            "SELECT MAX(number) FROM tasks WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        Ok(max.unwrap_or(0) + 1)
    }

    // ─── Transaction-aware variants (for use within save_state) ──────

    pub fn save_project_with_conn(
        &self,
        project: &CortexProject,
        tx: &Transaction,
    ) -> Result<()> {
        tx.execute(
            "INSERT OR REPLACE INTO projects (id, name, working_directory, status, position) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project.id,
                project.name,
                project.working_directory,
                project_status_to_str(&project.status),
                project.position,
            ],
        )?;
        Ok(())
    }

    pub fn save_task_with_conn(&self, task: &CortexTask, tx: &Transaction) -> Result<()> {
        tx.execute(
            "INSERT OR REPLACE INTO tasks (id, number, title, description, column_id, session_id, agent_type, agent_status, error_message, plan_output, pending_permission_count, pending_question_count, project_id, created_at, updated_at, entered_column_at, last_activity_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                task.id,
                task.number,
                task.title,
                task.description,
                task.column.0,
                task.session_id,
                task.agent_type.as_deref().unwrap_or("none"),
                task.agent_status.to_string(),
                task.error_message,
                task.plan_output,
                task.pending_permission_count,
                task.pending_question_count,
                task.project_id,
                task.created_at,
                task.updated_at,
                task.entered_column_at,
                task.last_activity_at,
            ],
        )?;
        Ok(())
    }

    pub fn save_kanban_order_with_conn(
        &self,
        column: &KanbanColumn,
        task_ids: &[String],
        tx: &Transaction,
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM kanban_order WHERE column_id = ?1",
            params![column.0],
        )?;
        for (pos, task_id) in task_ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO kanban_order (column_id, task_id, position) VALUES (?1, ?2, ?3)",
                params![column.0, task_id, pos],
            )?;
        }
        Ok(())
    }

    pub fn delete_task_with_conn(&self, task_id: &str, tx: &Transaction) -> Result<()> {
        tx.execute(
            "DELETE FROM kanban_order WHERE task_id = ?1",
            params![task_id],
        )?;
        tx.execute(
            "DELETE FROM tasks WHERE id = ?1",
            params![task_id],
        )?;
        Ok(())
    }

    /// Delete a project and all its associated data within a transaction.
    ///
    /// This is the transaction-aware counterpart to [`Self::delete_project`],
    /// designed for use inside `save_state()`. It removes:
    /// - Kanban order entries for tasks belonging to this project
    /// - All tasks belonging to this project
    /// - The task number counter metadata for this project
    /// - The project row itself
    pub fn delete_project_with_conn(&self, project_id: &str, tx: &Transaction) -> Result<()> {
        tx.execute(
            "DELETE FROM kanban_order WHERE task_id IN (SELECT id FROM tasks WHERE project_id = ?1)",
            params![project_id],
        )?;
        tx.execute("DELETE FROM tasks WHERE project_id = ?1", params![project_id])?;
        tx.execute(
            "DELETE FROM metadata WHERE key = ?1",
            params![format!("counter_{}", project_id)],
        )?;
        tx.execute("DELETE FROM projects WHERE id = ?1", params![project_id])?;
        Ok(())
    }

    pub fn set_metadata_with_conn(
        &self,
        key: &str,
        value: &str,
        tx: &Transaction,
    ) -> Result<()> {
        tx.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }
}

// ─── Schema & Migrations ──────────────────────────────────────────────────

/// Base schema (version 0). This is the initial set of tables created for
/// brand-new databases. All subsequent schema changes go through the
/// version-based migration system in `run_migrations()`.
///
/// NOTE: Do NOT add new columns here. Instead, bump `CURRENT_SCHEMA_VERSION`
/// and add a migration function.
const BASE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS _meta (
    key TEXT PRIMARY KEY,
    value TEXT
);

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    working_directory TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'disconnected',
    position INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    description TEXT DEFAULT '',
    column_id TEXT NOT NULL,
    session_id TEXT,
    agent_type TEXT DEFAULT 'none',
    agent_status TEXT DEFAULT 'pending',
    error_message TEXT,
    plan_output TEXT,
    pending_permission_count INTEGER DEFAULT 0,
    pending_question_count INTEGER DEFAULT 0,
    project_id TEXT NOT NULL REFERENCES projects(id),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    entered_column_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS kanban_order (
    column_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    position INTEGER NOT NULL,
    PRIMARY KEY (column_id, task_id)
);

CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT
);
"#;

/// Read the current schema version from the `_meta` table.
///
/// Returns 0 if the `_meta` table exists but has no version entry,
/// or if the database was created before the version-based migration system.
fn get_schema_version(conn: &Connection) -> Result<u32> {
    // Check if _meta table exists at all
    let has_meta: bool = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='_meta'")
        .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, String>(0)).map(|_| true))
        .unwrap_or(false);

    if !has_meta {
        return Ok(0);
    }

    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM _meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .ok();

    Ok(version.and_then(|v| v.parse().ok()).unwrap_or(0))
}

/// Write the schema version to the `_meta` table.
fn set_schema_version(conn: &Connection, version: u32) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO _meta (key, value) VALUES ('schema_version', ?1)",
        params![version.to_string()],
    )?;
    Ok(())
}

/// Detect the effective version of a pre-migration-system database by
/// inspecting which columns exist on the `tasks` table.
///
/// This provides backward compatibility for databases that were created
/// before the version-based migration system was introduced.
fn detect_schema_version(conn: &Connection) -> Result<u32> {
    // If `last_activity_at` exists on tasks, the DB is at least v1
    let has_last_activity: bool = conn
        .prepare("PRAGMA table_info(tasks)")
        .and_then(|mut stmt| {
            let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
            let columns: Vec<String> = rows.filter_map(|r| r.ok()).collect();
            Ok(columns.iter().any(|c| c == "last_activity_at"))
        })
        .unwrap_or(false);

    if has_last_activity {
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Run all pending schema migrations.
///
/// Migrations are applied sequentially. Each migration function is
/// responsible for upgrading the schema from version N to N+1.
fn run_migrations(conn: &Connection) -> Result<()> {
    let version = get_schema_version(conn)?;

    // If the DB has no version recorded, it might be a pre-existing database.
    // Detect its current state and set the version accordingly.
    if version == 0 {
        let detected = detect_schema_version(conn)?;
        if detected > 0 {
            set_schema_version(conn, detected)?;
        }
    }

    let mut current = get_schema_version(conn)?;

    if current < 1 {
        migrate_v0_to_v1(conn)?;
        current = 1;
        set_schema_version(conn, current)?;
    }

    // Future migrations go here:
    // if current < 2 { migrate_v1_to_v2(conn)?; current = 2; set_schema_version(conn, current)?; }

    Ok(())
}

/// Migration v0 → v1: Add `last_activity_at` column to `tasks` table.
fn migrate_v0_to_v1(conn: &Connection) -> Result<()> {
    conn.execute(
        "ALTER TABLE tasks ADD COLUMN last_activity_at INTEGER NOT NULL DEFAULT 0",
        [],
    )?;
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────

fn parse_agent_status(s: &str) -> AgentStatus {
    match s {
        "pending" => AgentStatus::Pending,
        "working" | "running" | "busy" => AgentStatus::Running,
        "hung" => AgentStatus::Hung,
        "ready" => AgentStatus::Ready,
        "done" | "complete" | "completed" | "idle" => AgentStatus::Complete,
        "failed" | "error" => AgentStatus::Error,
        _ => AgentStatus::Pending,
    }
}

fn parse_project_status(s: &str) -> ProjectStatus {
    match s {
        "disconnected" => ProjectStatus::Disconnected,
        "idle" => ProjectStatus::Idle,
        "working" => ProjectStatus::Working,
        "question" => ProjectStatus::Question,
        "done" => ProjectStatus::Done,
        "error" => ProjectStatus::Error,
        "hung" => ProjectStatus::Hung,
        _ => ProjectStatus::Disconnected,
    }
}

fn project_status_to_str(s: &ProjectStatus) -> &'static str {
    match s {
        ProjectStatus::Disconnected => "disconnected",
        ProjectStatus::Idle => "idle",
        ProjectStatus::Working => "working",
        ProjectStatus::Question => "question",
        ProjectStatus::Done => "done",
        ProjectStatus::Error => "error",
        ProjectStatus::Hung => "hung",
    }
}

/// Returns the default database path: `$XDG_DATA_HOME/cortex/cortex.db`.
///
/// Respects the `XDG_DATA_HOME` environment variable via `config::xdg_data_home()`,
/// ensuring the database ends up in a standard data directory.
pub fn default_db_path() -> std::path::PathBuf {
    crate::config::xdg_data_home()
        .join("cortex")
        .join("cortex.db")
}
