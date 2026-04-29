    use super::process_event;
    use crate::config::types::{ColumnConfig, ColumnsConfig};
    use crate::opencode::client::OpenCodeClient;
    use crate::state::types::*;
    use opencode_sdk_rs::resources::event::EventListResponse;

    /// Build a minimal `AppState` with a task that has a known session mapping.
    fn make_test_state() -> (AppState, String, String) {
        let mut state = AppState::default();
        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "Test Project".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
            ..Default::default()
        };
        state.add_project(project);
        state.project_registry.active_project_id = Some("proj-1".to_string());

        let task_id = "task-1".to_string();
        let session_id = "session-abc".to_string();
        let task = CortexTask {
            id: task_id.clone(),
            number: 1,
            title: "Test Task".to_string(),
            description: String::new(),
            column: KanbanColumn("planning".to_string()),
            session_id: Some(session_id.clone()),
            agent_type: Some("planning".to_string()),
            agent_status: AgentStatus::Running,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            planning_context: None,
            pending_description: None,
            queued_prompt: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        state.tasks.insert(task_id.clone(), task);
        state
            .kanban
            .columns
            .entry("planning".to_string())
            .or_default()
            .push(task_id.clone());
        state
            .session_tracker
            .session_to_task
            .insert(session_id.clone(), task_id.clone());

        // Tests that need a client construct their own; process_event only uses
        // the client for resolve_permission in the auto-approve path.

        (state, task_id, session_id)
    }

    /// Build a default `ColumnsConfig` with auto-progression on "planning" → "running".
    fn make_columns_config() -> ColumnsConfig {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: "todo".to_string(),
                    display_name: Some("Todo".to_string()),
                    visible: true,
                    agent: None,
                    auto_progress_to: None,
                },
                ColumnConfig {
                    id: "planning".to_string(),
                    display_name: Some("Plan".to_string()),
                    visible: true,
                    agent: Some("planning".to_string()),
                    auto_progress_to: Some("running".to_string()),
                },
                ColumnConfig {
                    id: "running".to_string(),
                    display_name: Some("Run".to_string()),
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: Some("review".to_string()),
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: Some("Review".to_string()),
                    visible: true,
                    agent: Some("reviewer-alpha".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        config.finalize();
        config
    }

    // ── SessionStatus ───────────────────────────────────────────────────

    #[test]
    fn session_status_running_updates_task() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "running" }),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );
        // render_dirty should be set
        assert!(state
            .dirty_flags
            .render_dirty
            .load(std::sync::atomic::Ordering::Relaxed));
        // No finalization for "running" status
        assert!(_finalize.is_none());
    }

    #[test]
    fn session_status_completed_updates_task() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        // Use a config without auto-progression so we can test the
        // status update in isolation (auto-progression fallback would
        // otherwise move the task and set Running).
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (_action, finalize) = process_event(&event, &mut state, &client, &columns_config);

        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
        // Completed status should signal finalization
        assert_eq!(finalize.as_deref(), Some(session_id.as_str()));
    }

    #[test]
    fn session_status_unknown_type_ignored() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Task starts as Running
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "something-weird" }),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should remain Running — unknown type is ignored
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );
    }

    // ── SessionIdle ─────────────────────────────────────────────────────

    #[test]
    fn session_idle_keeps_complete_for_auto_progress_column_and_shows_notification() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, finalize) = process_event(&event, &mut state, &client, &columns_config);

        let task = state.tasks.get(&task_id).unwrap();
        // Task was in "planning" which has auto_progress_to → stays Complete ("done"),
        // not Ready. The task will be auto-progressed to the next column.
        assert_eq!(task.agent_status, AgentStatus::Complete);
        // Task should have been auto-progressed to "running"
        assert_eq!(task.column.0, "running");
        // Notification should be set
        let notif = state.ui.notifications.back().unwrap();
        assert!(notif.message.contains("completed"));
        assert_eq!(notif.variant, NotificationVariant::Success);
        // SessionIdle should signal finalization
        assert_eq!(finalize.as_deref(), Some(session_id.as_str()));
    }

    #[test]
    fn session_idle_triggers_auto_progression() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Task starts in "planning", config has auto_progress_to "running"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "planning");

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should have triggered auto-progression
        assert!(action.is_some());
        // Task should have moved to "running"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        // Should be in the running column in kanban
        assert!(state
            .kanban
            .columns
            .get("running")
            .unwrap()
            .contains(&task_id));
        // Should be removed from planning column
        assert!(!state
            .kanban
            .columns
            .get("planning")
            .unwrap()
            .contains(&task_id));
    }

    #[test]
    fn session_idle_no_auto_progress_when_not_configured() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Config without auto-progression for "planning"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "planning");

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task should still be in "planning"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "planning");
    }

    #[test]
    fn session_idle_unknown_session_ignored() {
        let (mut state, task_id, _session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: "nonexistent-session".to_string(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task should still be Running (no change)
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );
        // No notification should be set
        assert!(state.ui.notifications.is_empty());
    }

    // ── SessionError ────────────────────────────────────────────────────

    #[test]
    fn session_error_records_error_on_task() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::SessionError {
            properties: opencode_sdk_rs::resources::event::SessionErrorProps {
                error: Some(
                    opencode_sdk_rs::resources::shared::SessionError::UnknownError {
                        data: opencode_sdk_rs::resources::shared::UnknownErrorData {
                            message: "something broke".to_string(),
                        },
                    },
                ),
                session_id: Some(session_id.clone()),
            },
        };
        process_event(&event, &mut state, &client, &columns_config);

        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.agent_status, AgentStatus::Error);
        assert!(task
            .error_message
            .as_ref()
            .unwrap()
            .contains("something broke"));
    }

    // ── MessagePartDelta ────────────────────────────────────────────────

    #[test]
    fn message_part_delta_appends_streaming_text() {
        let (mut state, _task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "Hello ".to_string(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello "));

        // Append more text
        let event2 = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "World".to_string(),
            },
        };
        let (_action, _finalize) = process_event(&event2, &mut state, &client, &columns_config);

        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello World"));
    }

    // ── PermissionAsked ─────────────────────────────────────────────────

    #[test]
    fn permission_asked_creates_pending_request() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::PermissionAsked {
            properties: serde_json::json!({
                "id": "perm-001",
                "sessionID": session_id,
                "tool": "bash",
                "title": "Run build command"
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should have a pending permission on the task
        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.pending_permission_count, 1);

        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
        assert_eq!(session.pending_permissions.len(), 1);
        assert_eq!(session.pending_permissions[0].id, "perm-001");
        assert_eq!(session.pending_permissions[0].tool_name, "bash");
    }

    // ── PermissionReplied ───────────────────────────────────────────────

    #[test]
    fn permission_replied_resolves_request() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // First add a pending permission
        state.add_permission_request(
            &task_id,
            PermissionRequest {
                id: "perm-001".to_string(),
                session_id: session_id.clone(),
                tool_name: "bash".to_string(),
                description: "Run cmd".to_string(),
                status: "pending".to_string(),
                details: None,
            },
        );
        assert_eq!(
            state.tasks.get(&task_id).unwrap().pending_permission_count,
            1
        );

        let event = EventListResponse::PermissionReplied {
            properties: opencode_sdk_rs::resources::event::PermissionRepliedProps {
                session_id: session_id.clone(),
                request_id: "perm-001".to_string(),
                reply: opencode_sdk_rs::resources::event::PermissionReply::Once,
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Permission should be resolved (count back to 0)
        assert_eq!(
            state.tasks.get(&task_id).unwrap().pending_permission_count,
            0
        );
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
        assert!(session.pending_permissions.is_empty());
    }

    // ── QuestionAsked ───────────────────────────────────────────────────

    #[test]
    fn question_asked_sets_warning_notification() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();
        let event = EventListResponse::QuestionAsked {
            properties: serde_json::json!({
                "sessionID": session_id,
                "id": "q-001",
                "question": "Which approach should I use for the refactoring?"
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        let notif = state.ui.notifications.back().unwrap();
        assert!(notif.message.contains("Question pending"));
        assert!(notif.message.contains("Which approach"));
        assert_eq!(notif.variant, NotificationVariant::Warning);
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
        assert_eq!(session.pending_questions.len(), 1);
        assert_eq!(session.pending_questions[0].id, "q-001");
        assert_eq!(
            session.pending_questions[0].question,
            "Which approach should I use for the refactoring?"
        );
        assert_eq!(session.pending_questions[0].status, "pending");
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 1);
    }

    #[test]
    fn question_asked_stores_answer_options() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();
        let event = EventListResponse::QuestionAsked {
            properties: serde_json::json!({
                "sessionID": session_id,
                "id": "q-002",
                "question": "Which approach should I use?",
                "answers": ["Option A", "Option B", "Option C"]
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
        assert_eq!(session.pending_questions.len(), 1);
        assert_eq!(
            session.pending_questions[0].answers,
            vec!["Option A", "Option B", "Option C"]
        );
    }

    #[test]
    fn question_asked_truncates_long_question() {
        let (mut state, _task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();
        let long_question = "a".repeat(100);
        let event = EventListResponse::QuestionAsked {
            properties: serde_json::json!({
                "sessionID": session_id,
                "id": "q-003",
                "question": long_question
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        let notif = state.ui.notifications.back().unwrap();
        assert!(notif.message.len() < long_question.len() + 30);
    }

    #[test]
    fn question_replied_removes_from_pending() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();
        let event = EventListResponse::QuestionAsked {
            properties: serde_json::json!({
                "sessionID": session_id,
                "id": "q-004",
                "question": "Should I proceed?",
                "answers": ["Yes", "No"]
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 1);
        let reply_event = EventListResponse::QuestionReplied {
            properties: opencode_sdk_rs::resources::event::QuestionRepliedProps {
                session_id: session_id.clone(),
                request_id: "q-004".to_string(),
                answers: vec![],
            },
        };
        let (_action, _finalize) =
            process_event(&reply_event, &mut state, &client, &columns_config);
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
        assert!(session.pending_questions.is_empty());
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 0);
    }

    // ── Ignored events ──────────────────────────────────────────────────

    #[test]
    fn ignored_events_do_not_panic() {
        let (mut state, _task_id, _session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Events we don't handle should not panic
        let event = EventListResponse::FileEdited {
            properties: opencode_sdk_rs::resources::event::FileEditedProps {
                file: "src/main.rs".to_string(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        let event = EventListResponse::ServerConnected {
            properties: opencode_sdk_rs::resources::event::EmptyProps {},
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // render_dirty should still be set
        assert!(state
            .dirty_flags
            .render_dirty
            .load(std::sync::atomic::Ordering::Relaxed));
    }

    // ── render_dirty always set ─────────────────────────────────────────

    #[test]
    fn process_event_always_marks_render_dirty() {
        let (mut state, _task_id, _session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Clear the flag first
        state
            .dirty_flags
            .render_dirty
            .store(false, std::sync::atomic::Ordering::Relaxed);

        let event = EventListResponse::ServerConnected {
            properties: opencode_sdk_rs::resources::event::EmptyProps {},
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        assert!(state
            .dirty_flags
            .render_dirty
            .load(std::sync::atomic::Ordering::Relaxed));
    }

    // ── Race condition fix: stale SessionStatus can't overwrite Running ───

    #[test]
    fn stale_session_status_does_not_overwrite_after_mapping_cleared() {
        // Simulate the core invariant of the race condition fix:
        // After the session→task mapping is cleared (by auto-progression),
        // a stale SessionStatus "complete" for the OLD session cannot find
        // the task and therefore cannot overwrite its status.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Simulate auto-progression clearing the session mapping
        state.session_tracker.session_to_task.remove(&session_id);

        // Set the task to Running (as auto-progression would)
        state.update_task_agent_status(&task_id, AgentStatus::Running);

        // Now a stale SessionStatus "complete" arrives for the old session
        let stale_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (_action, _finalize) =
            process_event(&stale_event, &mut state, &client, &columns_config);

        // Task should still be Running — the stale event couldn't find it
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running,
            "Stale SessionStatus should not overwrite Running when mapping is cleared"
        );
    }

    #[test]
    fn session_mapping_present_allows_status_update() {
        // Control test: when the session→task mapping IS present, SessionStatus
        // updates the task as expected.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        // Disable auto-progression so we test pure status update.
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        // Mapping is present (from make_test_state)
        assert!(state.get_task_id_by_session(&session_id).is_some());

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task status should be updated to Complete
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete,
        );
    }

    // ── Ready vs Complete status ─────────────────────────────────────────

    #[test]
    fn terminal_column_gets_complete_not_ready() {
        // When a task is in a terminal column (no auto_progress_to), SessionIdle
        // should set Complete, not Ready.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Config without auto-progression for "planning" or "running"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;
        columns_config.definitions[2].auto_progress_to = None;

        // Move task to "running" (terminal column — no auto_progress_to)
        state.move_task(&task_id, KanbanColumn("running".to_string()));

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Terminal column should get Complete, not Ready
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
    }

    // ── Ready status from plan_output ────────────────────────────────────

    #[test]
    fn terminal_column_with_plan_output_gets_ready() {
        // A task in a terminal column (no auto_progress_to) should get Ready
        // ("ready") when it has a non-empty plan_output — the plan signals
        // there's more work to do.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1").unwrap();

        // Config without auto-progression for "planning"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        // Pre-set plan_output on the task (simulating what extract_plan_output does)
        state.tasks.get_mut(&task_id).unwrap().plan_output =
            Some("Here is the plan:\n1. Do X\n2. Do Y".to_string());

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should get Ready — plan_output triggers Ready even in terminal columns.
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Ready
        );
    }

    #[test]
    fn terminal_column_without_plan_output_gets_complete() {
        // A task in a terminal column (no auto_progress_to) WITHOUT plan_output
        // should get Complete (not Ready).
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Config without auto-progression for "planning" or "running"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;
        columns_config.definitions[2].auto_progress_to = None;

        // Move task to "running" (terminal column — no auto_progress_to)
        state.move_task(&task_id, KanbanColumn("running".to_string()));

        // No plan_output set — task.plan_output is None

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Terminal column without plan_output → Complete
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
    }

    #[test]
    fn empty_plan_output_does_not_trigger_ready() {
        // An empty string plan_output should NOT trigger Ready status.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Config without auto-progression for "planning"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        // Set an empty plan_output
        state.tasks.get_mut(&task_id).unwrap().plan_output = Some(String::new());

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Empty plan_output should not trigger Ready
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
    }

    #[test]
    fn auto_progress_column_with_plan_output_stays_complete() {
        // A task in a column WITH auto_progress_to AND plan_output should
        // stay Complete ("done") — auto_progress takes priority over plan_output
        // so the task shows "done" before being moved to the next column.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Use default config: "planning" has auto_progress_to → "running"
        let columns_config = make_columns_config();

        // Pre-set plan_output on the task
        state.tasks.get_mut(&task_id).unwrap().plan_output =
            Some("Step 1: Refactor module\nStep 2: Add tests".to_string());

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should stay Complete — auto_progress takes priority, keeping "done" status.
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
    }

    // ── Plan output extraction ───────────────────────────────────────────

    #[test]
    fn extract_plan_output_from_messages() {
        let (mut state, task_id, _session_id) = make_test_state();

        // Add messages to the session
        state
            .session_tracker
            .task_sessions
            .entry(task_id.clone())
            .or_default()
            .messages = vec![
            TaskMessage {
                id: "msg-1".to_string(),
                role: MessageRole::User,
                parts: vec![TaskMessagePart::Text {
                    text: "Plan this".to_string(),
                }],
                created_at: None,
            },
            TaskMessage {
                id: "msg-2".to_string(),
                role: MessageRole::Assistant,
                parts: vec![TaskMessagePart::Text {
                    text: "Here is the plan:\n1. Do X\n2. Do Y".to_string(),
                }],
                created_at: None,
            },
        ];

        state.extract_plan_output(&task_id);

        let task = state.tasks.get(&task_id).unwrap();
        assert!(task.plan_output.is_some());
        let plan = task.plan_output.as_ref().unwrap();
        assert!(plan.contains("Here is the plan"));
        assert!(plan.contains("Do X"));
        assert!(plan.contains("Do Y"));
    }

    #[test]
    fn extract_plan_output_from_streaming_text() {
        let (mut state, task_id, _session_id) = make_test_state();

        // Add streaming text (no finalized messages)
        let session = state
            .session_tracker
            .task_sessions
            .entry(task_id.clone())
            .or_default();
        session.streaming_text = Some("Streaming plan output...".to_string());

        state.extract_plan_output(&task_id);

        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(
            task.plan_output.as_deref(),
            Some("Streaming plan output...")
        );
    }

    #[test]
    fn extract_plan_output_noop_when_no_session() {
        let (mut state, task_id, _session_id) = make_test_state();

        // No session data at all
        state.extract_plan_output(&task_id);

        let task = state.tasks.get(&task_id).unwrap();
        assert!(task.plan_output.is_none());
    }

    #[test]
    fn extract_plan_output_marks_task_dirty() {
        let (mut state, task_id, _session_id) = make_test_state();

        // Clear dirty flag
        state.dirty_flags.dirty_tasks.clear();

        state
            .session_tracker
            .task_sessions
            .entry(task_id.clone())
            .or_default()
            .messages = vec![TaskMessage {
            id: "msg-1".to_string(),
            role: MessageRole::Assistant,
            parts: vec![TaskMessagePart::Text {
                text: "Plan".to_string(),
            }],
            created_at: None,
        }];

        state.extract_plan_output(&task_id);

        assert!(state.dirty_flags.dirty_tasks.contains(&task_id));
    }

    // ── Integration-style tests: full event lifecycle ──────────────────

    /// Test the full lifecycle of an agent session through SSE events:
    /// status:running → delta → delta → status:completed → session:idle.
    ///
    /// Verifies that:
    /// - Streaming text accumulates correctly from deltas
    /// - Status transitions are correct
    /// - Finalization session ID is signaled
    /// - Auto-progression moves the task to the next column
    #[test]
    fn integration_full_agent_lifecycle() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // 1. Session starts running
        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "running" }),
            },
        };
        let (_action, finalize) = process_event(&event, &mut state, &client, &columns_config);
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );
        assert!(finalize.is_none());

        // 2. Receive streaming deltas
        for delta in &["Hello ", "world", "! This is a test."] {
            let delta_event = EventListResponse::MessagePartDelta {
                properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                    session_id: session_id.clone(),
                    message_id: "msg-1".to_string(),
                    part_id: "part-1".to_string(),
                    field: "text".to_string(),
                    delta: delta.to_string(),
                },
            };
            process_event(&delta_event, &mut state, &client, &columns_config);
        }

        // Verify streaming text accumulated
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
        assert_eq!(
            session.streaming_text.as_deref(),
            Some("Hello world! This is a test.")
        );

        // 3. Session completes — auto-progression fallback triggers,
        // moving task from "planning" → "running" and returning an action.
        let complete_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (action, finalize) =
            process_event(&complete_event, &mut state, &client, &columns_config);

        // Task should be in "running" column now (auto-progressed from "planning")
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        // Should have an auto-progress action
        assert!(action.is_some());
        // Should signal finalization
        assert_eq!(finalize.as_deref(), Some(session_id.as_str()));

        // Simulate the event loop post-processing that the real code does:
        // clear the old session mapping and set Running status.
        {
            let old_sid = state.tasks.get(&task_id).and_then(|t| t.session_id.clone());
            if let Some(old_sid) = old_sid {
                state.session_tracker.session_to_task.remove(&old_sid);
            }
            state.update_task_agent_status(&task_id, AgentStatus::Running);
        }

        // 4. SessionIdle arrives later — session mapping was cleared,
        // so process_session_idle won't find the task → no action.
        let idle_event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (idle_action, idle_finalize) =
            process_event(&idle_event, &mut state, &client, &columns_config);

        // No action should be triggered (mapping was cleared)
        assert!(idle_action.is_none());
        // Finalization may still be signaled
        assert!(idle_finalize.is_some());
    }

    /// Test SSE deduplication: receiving the exact same delta twice (same key,
    /// same content) should not double the streaming text. This simulates
    /// what happens when concurrent SSE connections deliver the same event.
    ///
    /// Also tests that replaying an old part (different key that was already
    /// seen) is correctly skipped.
    #[test]
    fn integration_dedup_prevents_text_doubling() {
        let (mut state, _task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // 1. Send first delta for part-1
        let delta1 = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "Hello ".to_string(),
            },
        };
        process_event(&delta1, &mut state, &client, &columns_config);

        // 2. Send continuation delta for part-1 (same key, different content)
        let delta2 = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "World".to_string(),
            },
        };
        process_event(&delta2, &mut state, &client, &columns_config);

        // Verify accumulated text
        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello World"));

        // 3. Simulate concurrent SSE loop delivering the exact same last delta
        // (same key, same content) — defense-in-depth dedup should catch this
        let delta_dup = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "World".to_string(), // Same content as last delta
            },
        };
        process_event(&delta_dup, &mut state, &client, &columns_config);

        // Text should NOT have doubled
        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello World"));

        // 4. Now send a genuinely NEW part — should be accepted
        let delta_new = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-2".to_string(),
                field: "text".to_string(),
                delta: " More text".to_string(),
            },
        };
        process_event(&delta_new, &mut state, &client, &columns_config);

        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(
            session.streaming_text.as_deref(),
            Some("Hello World More text")
        );

        // 5. Replay an old part that was already seen (different from current)
        // This simulates server replaying events after reconnection.
        // Since the key ("msg-1", "part-1") is in seen_delta_keys and
        // is NOT the current continuation, it's correctly identified as
        // a replay and skipped.
        let delta_old_replay = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "Old replay".to_string(),
            },
        };
        process_event(&delta_old_replay, &mut state, &client, &columns_config);

        // Old replay should be skipped — text unchanged
        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(
            session.streaming_text.as_deref(),
            Some("Hello World More text")
        );
    }

    /// Test error recovery: a session error should mark the task as Error
    /// and allow a new session to be started afterward.
    #[test]
    fn integration_error_then_restart() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // 1. Agent starts running
        let running_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "running" }),
            },
        };
        process_event(&running_event, &mut state, &client, &columns_config);
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );

        // 2. Agent errors
        let error_event = EventListResponse::SessionError {
            properties: opencode_sdk_rs::resources::event::SessionErrorProps {
                error: Some(
                    opencode_sdk_rs::resources::shared::SessionError::UnknownError {
                        data: opencode_sdk_rs::resources::shared::UnknownErrorData {
                            message: "API rate limit exceeded".to_string(),
                        },
                    },
                ),
                session_id: Some(session_id.clone()),
            },
        };
        process_event(&error_event, &mut state, &client, &columns_config);

        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.agent_status, AgentStatus::Error);
        assert!(task
            .error_message
            .as_ref()
            .unwrap()
            .contains("API rate limit exceeded"));

        // 3. A new session can be started (simulate by setting Running again)
        state.update_task_agent_status(&task_id, AgentStatus::Running);
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );
    }

    // ── Task 4.1: planning→do auto-progression preserves context ──────────

    #[test]
    fn planning_to_do_preserves_plan_output() {
        // Simulate the full planning→do auto-progression flow:
        // 1. Planning agent streams text
        // 2. Session completes → extract_plan_output called
        // 3. Session idle → auto-progression to "running" column
        // 4. New session created → session data cleared but plan_output preserved
        // 5. build_prompt_for_agent includes the plan
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1").unwrap();
        let columns_config = make_columns_config();

        // 1. Planning agent streams a plan
        let plan_text = "Step 1: Analyze codebase\nStep 2: Refactor module X\nStep 3: Add tests";
        for delta in plan_text.split_inclusive('\n') {
            let delta_event = EventListResponse::MessagePartDelta {
                properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                    session_id: session_id.clone(),
                    message_id: "msg-1".to_string(),
                    part_id: "part-1".to_string(),
                    field: "text".to_string(),
                    delta: delta.to_string(),
                },
            };
            process_event(&delta_event, &mut state, &client, &columns_config);
        }

        // 2. Session completes → extract_plan_output called (via SessionStatus "completed")
        let complete_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (complete_action, _finalize) =
            process_event(&complete_event, &mut state, &client, &columns_config);

        // Simulate the event loop post-processing: clear old session mapping
        // and set Running status (what the real event loop does at lines 139-151).
        if complete_action.is_some() {
            let old_sid = state.tasks.get(&task_id).and_then(|t| t.session_id.clone());
            if let Some(old_sid) = old_sid {
                state.session_tracker.session_to_task.remove(&old_sid);
            }
            state.update_task_agent_status(&task_id, AgentStatus::Running);
        }

        // Verify plan_output was extracted on SessionStatus "completed"
        assert!(
            state.tasks.get(&task_id).unwrap().plan_output.is_some(),
            "plan_output should be extracted on SessionStatus completed"
        );

        // Verify auto-progression happened on SessionStatus "completed"
        assert!(
            complete_action.is_some(),
            "auto-progression should trigger on SessionStatus completed"
        );
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");

        // 3. Session idle arrives later — mapping was cleared, so no-op
        let idle_event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (idle_action, _idle_finalize) =
            process_event(&idle_event, &mut state, &client, &columns_config);

        // No action — mapping was cleared by post-processing above
        assert!(
            idle_action.is_none(),
            "SessionIdle should be no-op after auto-progression"
        );

        // 4. Simulate start_agent creating a new session (clearing session data)
        state.set_task_session_id(&task_id, Some("session-do-agent".to_string()));
        state.clear_session_data(&task_id);

        // 5. Verify plan_output is STILL preserved after session data clearing
        let task = state.tasks.get(&task_id).unwrap();
        assert!(
            task.plan_output.is_some(),
            "plan_output must survive session data clearing"
        );
        let plan = task.plan_output.as_ref().unwrap();
        assert!(plan.contains("Step 1: Analyze codebase"));
        assert!(plan.contains("Step 2: Refactor module X"));

        // 6. Verify build_prompt_for_agent includes the plan
        let prompt = OpenCodeClient::build_prompt_for_agent(task, "do", None);
        assert!(prompt.contains("## Plan (from planning phase)"));
        assert!(prompt.contains("Step 1: Analyze codebase"));
    }

    // ── Task 4.2: manual planning→do move preserves context ───────────────

    #[test]
    fn manual_move_preserves_plan_output() {
        // Simulate a manual move from planning to running column:
        // plan_output should be preserved through the transition.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1").unwrap();
        let columns_config = make_columns_config();

        // Planning agent streams a plan
        let plan_text = "My plan: refactor the parser";
        let delta_event = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: plan_text.to_string(),
            },
        };
        process_event(&delta_event, &mut state, &client, &columns_config);

        // Extract plan
        state.extract_plan_output(&task_id);
        assert!(state.tasks.get(&task_id).unwrap().plan_output.is_some());

        // Manually move task to running column
        state.move_task(&task_id, KanbanColumn("running".to_string()));

        // Plan should still be there
        assert!(
            state.tasks.get(&task_id).unwrap().plan_output.is_some(),
            "plan_output should survive manual column move"
        );
    }

    // ── Task 4.4: extract_plan_output edge cases ──────────────────────────

    #[test]
    fn extract_plan_output_preserves_existing_when_empty() {
        // If a task already has plan_output and extraction yields empty,
        // the existing value should be preserved.
        let (mut state, task_id, _session_id) = make_test_state();

        // Pre-set a plan_output
        state.tasks.get_mut(&task_id).unwrap().plan_output =
            Some("Existing plan from previous extraction".to_string());

        // Create session with empty streaming_text
        let session = state
            .session_tracker
            .task_sessions
            .entry(task_id.clone())
            .or_default();
        session.streaming_text = Some("   ".to_string()); // whitespace only

        state.extract_plan_output(&task_id);

        // Should preserve the existing plan_output
        assert_eq!(
            state.tasks.get(&task_id).unwrap().plan_output.as_deref(),
            Some("Existing plan from previous extraction"),
            "existing plan_output should not be overwritten by empty extraction"
        );
    }

    #[test]
    fn extract_plan_output_creates_lazy_session_entry() {
        // If no session data exists but the task does, a lazy entry should be created.
        let (mut state, task_id, _session_id) = make_test_state();

        // No session data exists yet
        assert!(!state.session_tracker.task_sessions.contains_key(&task_id));

        // Extract should not panic and should create a lazy entry
        state.extract_plan_output(&task_id);

        // A lazy session entry should now exist
        assert!(
            state.session_tracker.task_sessions.contains_key(&task_id),
            "lazy session entry should be created for existing task"
        );
        // plan_output should still be None (no data to extract)
        assert!(state.tasks.get(&task_id).unwrap().plan_output.is_none());
    }

    #[test]
    fn extract_plan_output_from_messages_over_streaming() {
        // Messages should take priority over streaming_text.
        let (mut state, task_id, _session_id) = make_test_state();

        let session = state
            .session_tracker
            .task_sessions
            .entry(task_id.clone())
            .or_default();
        session.streaming_text = Some("Streaming fallback text".to_string());
        session.messages = vec![TaskMessage {
            id: "msg-1".to_string(),
            role: MessageRole::Assistant,
            parts: vec![TaskMessagePart::Text {
                text: "Rich plan from messages".to_string(),
            }],
            created_at: None,
        }];

        state.extract_plan_output(&task_id);

        let plan = state
            .tasks
            .get(&task_id)
            .unwrap()
            .plan_output
            .as_ref()
            .unwrap();
        assert_eq!(plan, "Rich plan from messages");
        assert!(!plan.contains("Streaming fallback"));
    }

    #[test]
    fn session_status_complete_extracts_plan_early() {
        // SessionStatus "complete" should trigger plan extraction
        // even before SessionIdle arrives.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1").unwrap();
        let columns_config = make_columns_config();

        // Stream some plan text
        let delta_event = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "Early plan extraction test".to_string(),
            },
        };
        process_event(&delta_event, &mut state, &client, &columns_config);

        // Send SessionStatus "completed" (NOT SessionIdle)
        let complete_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        process_event(&complete_event, &mut state, &client, &columns_config);

        // Plan should be extracted from streaming_text already
        assert!(
            state.tasks.get(&task_id).unwrap().plan_output.is_some(),
            "plan_output should be extracted on SessionStatus completed (before SessionIdle)"
        );
        assert_eq!(
            state.tasks.get(&task_id).unwrap().plan_output.as_deref(),
            Some("Early plan extraction test")
        );
    }

    // ── Question Status (pending questions block auto-progression) ──────

    #[test]
    fn session_idle_with_pending_questions_sets_question_status() {
        // When a task has pending questions and the agent goes idle,
        // the task should enter Question status instead of Ready/Complete,
        // and auto-progression should be blocked.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Simulate a pending question on the task
        state
            .tasks
            .get_mut(&task_id)
            .unwrap()
            .pending_question_count = 1;

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (action, finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task should be in Question status, not Ready
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Question,
            "task with pending questions should enter Question status on SessionIdle"
        );
        // No auto-progression action should be returned
        assert!(
            action.is_none(),
            "auto-progression should be blocked when questions are pending"
        );
        // Finalization should still happen
        assert_eq!(finalize.as_deref(), Some(session_id.as_str()));
    }

    #[test]
    fn session_status_complete_with_pending_questions_sets_question_status() {
        // SessionStatus "complete" should also set Question status when
        // pending questions exist, blocking auto-progression.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Simulate a pending question on the task
        state
            .tasks
            .get_mut(&task_id)
            .unwrap()
            .pending_question_count = 2;

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (action, finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task should be in Question status
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Question,
            "task with pending questions should enter Question status on SessionStatus complete"
        );
        // No auto-progression
        assert!(action.is_none());
        // Finalization should still happen
        assert!(finalize.is_some());
    }

    #[test]
    fn session_idle_without_pending_questions_proceeds_normally() {
        // When no questions are pending, the existing Complete ("done") +
        // auto-progression behavior should be preserved.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // No pending questions — default behavior
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 0);

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should be Complete ("done") — auto_progress keeps Complete instead of Ready
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete,
            "task without pending questions should stay Complete when auto_progress is configured"
        );
    }

    // ─── Additional edge cases ───────────────────────────────────────────

    #[test]
    fn interleaved_events_from_multiple_sessions() {
        // Simulate events arriving from two different agent sessions
        // that share the same task (e.g., a main agent and subagent).
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();
        let sub_session_id = "sub-session-xyz".to_string();

        // Register a subagent session
        state.register_subagent_session(&task_id, &sub_session_id, "do");

        // Main session is running
        let event_running = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({"type": "running"}),
            },
        };
        let (_action, _finalize) = process_event(&event_running, &mut state, &client, &columns_config);
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );

        // Sub-session completes — should not affect the main task status
        let event_sub_complete = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: sub_session_id.clone(),
                status: serde_json::json!({"type": "complete"}),
            },
        };
        let (_action, finalize) = process_event(&event_sub_complete, &mut state, &client, &columns_config);
        // Main task should still be Running
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running,
            "sub-agent completion should not affect main task status"
        );
        // finalize should be Some (sub-session completed)
        assert!(finalize.is_some());
    }

    #[test]
    fn unknown_session_id_in_permission_replied_ignored() {
        let (mut state, _task_id, _session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Permission reply for a session that doesn't exist
        let event = EventListResponse::PermissionReplied {
            properties: opencode_sdk_rs::resources::event::PermissionRepliedProps {
                session_id: "nonexistent-session".to_string(),
                request_id: "perm-1".to_string(),
                reply: opencode_sdk_rs::resources::event::PermissionReply::Once,
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        // Should not panic — just silently ignored
    }

    #[test]
    fn rapid_status_changes_last_one_wins() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let mut columns_config = make_columns_config();
        // Disable auto-progression so we can test pure status updates
        columns_config.definitions[1].auto_progress_to = None;

        // Rapid-fire status changes: running → complete → running → complete
        for status in &["running", "complete", "running", "complete"] {
            let event = EventListResponse::SessionStatus {
                properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                    session_id: session_id.clone(),
                    status: serde_json::json!({"type": status}),
                },
            };
            let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        }

        // Final status should be Complete (last one wins)
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete,
            "last status update should win"
        );
    }

    #[test]
    fn message_delta_for_unknown_session_ignored() {
        let (mut state, _task_id, _session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Message delta for a session with no task mapping
        let event = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: "unknown-session".to_string(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "content".to_string(),
                delta: "some text".to_string(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        // Should not panic — silently ignored
    }
