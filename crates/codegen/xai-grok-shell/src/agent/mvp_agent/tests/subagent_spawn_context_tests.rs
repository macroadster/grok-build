//! Subagent spawn-context inheritance: a child session must inherit the parent's
//! permission handle and goal-loop gate so policy and run-state can't be bypassed
//! by delegating to a subagent.

use super::{build_minimal_agent_for_tests, make_test_handle};
use agent_client_protocol as acp;
use xai_acp_lib::AcpAgentGatewaySender as GatewaySender;

/// Subagents inherit the parent permission handle, so a managed `Read(**/.env)`
/// deny still blocks the child — direct read and the `cat .env` shell equivalent.
#[tokio::test]
async fn subagent_spawn_context_inherits_parent_permission_handle() {
    use xai_grok_workspace::permission::types::{
        PatternMode, PermissionConfig, PermissionRule, RuleAction, ToolFilter,
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let agent = build_minimal_agent_for_tests();
            let sid = acp::SessionId::new("parent-permission");
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let gateway = GatewaySender::new(tx);
            let cwd = xai_grok_paths::AbsPathBuf::new(std::path::PathBuf::from("/tmp"))
                .expect("absolute cwd");
            let (permission_handle, _events_rx) =
                xai_grok_workspace::permission::spawn_permission_manager(
                    sid.clone(),
                    gateway,
                    cwd,
                    xai_grok_workspace::permission::types::ClientType::Generic,
                    Some(PermissionConfig::new(vec![PermissionRule {
                        action: RuleAction::Deny,
                        tool: ToolFilter::Read,
                        pattern: Some("**/.env".to_owned()),
                        pattern_mode: PatternMode::Glob,
                    }])),
                    Vec::new(), // deny_read_globs
                    Vec::new(),
                    false,
                    None,
                );

            let mut handle = make_test_handle("test-model", false, None);
            handle.permission_handle = permission_handle;
            agent.sessions.borrow_mut().insert(sid.clone(), handle);

            let ctx = agent.build_subagent_spawn_context(sid.0.as_ref());
            let inherited = ctx
                .permission_handle
                .expect("subagent context must inherit parent permission handle");

            // Direct file read and the shell equivalent both hit the parent deny.
            for access in [
                xai_grok_workspace::permission::AccessKind::Read(Some(".env".into())),
                xai_grok_workspace::permission::AccessKind::Bash("cat .env".into()),
            ] {
                let decision = inherited
                    .request(
                        access.clone(),
                        acp::ToolCallUpdate::new(acp::ToolCallId::new("tc"), Default::default()),
                        Some("child-session".to_owned()),
                        Some("general-purpose".to_owned()),
                        Some("permission inheritance regression".to_owned()),
                    )
                    .await;
                assert!(
                    matches!(
                        decision,
                        xai_grok_workspace::permission::Decision::PolicyDeny(_)
                    ),
                    "subagent-inherited handle must enforce parent deny for {access:?}, got {decision:?}"
                );
            }
        })
        .await;
}

/// A subagent shares the parent's `goal_loop_active_gate` Arc, so flipping the
/// parent gate is observed through the child context (same allocation).
#[tokio::test]
async fn subagent_spawn_context_shares_parent_goal_loop_gate() {
    use std::sync::atomic::Ordering::Relaxed;

    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("parent-goal");
    let handle = make_test_handle("test-model", false, None);
    // Clone the parent's live gate before the handle moves into `sessions`.
    let parent_gate = handle.tool_context.goal_loop_active_gate.clone();
    agent.sessions.borrow_mut().insert(sid.clone(), handle);

    let ctx = agent.build_subagent_spawn_context(sid.0.as_ref());

    // Flipping the parent gate must surface through the child flag (shared Arc).
    assert!(!ctx.goal_loop_active.load(Relaxed));
    parent_gate.store(true, Relaxed);
    assert!(
        ctx.goal_loop_active.load(Relaxed),
        "subagent context must observe the parent's goal-loop gate (same Arc)"
    );
}

/// A subagent inherits the parent session's `ask_user_question` gate, so
/// `--no-ask-user` strips the tool from subagents too, while the default keeps it.
#[tokio::test]
async fn subagent_spawn_context_inherits_parent_ask_user_question_gate() {
    let agent = build_minimal_agent_for_tests();

    // Parent with the tool disabled (the `--no-ask-user` case) → child off.
    let sid_off = acp::SessionId::new("parent-no-ask");
    let mut handle_off = make_test_handle("test-model", false, None);
    handle_off.ask_user_question_enabled = false;
    agent
        .sessions
        .borrow_mut()
        .insert(sid_off.clone(), handle_off);
    let ctx_off = agent.build_subagent_spawn_context(sid_off.0.as_ref());
    assert!(
        !ctx_off.ask_user_question_enabled,
        "subagent must inherit the parent's disabled ask_user_question gate (--no-ask-user)"
    );

    // Parent with the tool enabled (the default) → child on.
    let sid_on = acp::SessionId::new("parent-ask");
    let handle_on = make_test_handle("test-model", false, None);
    agent
        .sessions
        .borrow_mut()
        .insert(sid_on.clone(), handle_on);
    let ctx_on = agent.build_subagent_spawn_context(sid_on.0.as_ref());
    assert!(
        ctx_on.ask_user_question_enabled,
        "subagent must inherit the parent's enabled ask_user_question gate"
    );
}

/// Manager (and other depth-1) subagents are tracked only in the subagent
/// coordinator — not `MvpAgent.sessions`. Nested spawns (manager → worker)
/// must resolve the parent handle from that map or they panic the ACP worker.
#[tokio::test]
async fn nested_subagent_parent_resolves_from_coordinator() {
    use crate::agent::subagent::SubagentTracker;

    let agent = build_minimal_agent_for_tests();
    let manager_id = "manager-subagent-1";

    // Not inserted into `agent.sessions` — mirrors a real manager subagent.
    let mut manager_handle = make_test_handle("test-model", false, None);
    manager_handle.info.id = acp::SessionId::new(manager_id);
    manager_handle.info.cwd = "/tmp/manager-cwd".to_string();
    manager_handle.agent_name = "manager".to_string();
    manager_handle.tool_context.subagent_depth = 1;
    let parent_gate = manager_handle.tool_context.goal_loop_active_gate.clone();

    agent.subagent_coordinator.borrow_mut().insert(SubagentTracker {
        subagent_id: manager_id.into(),
        parent_session_id: "top-level-session".into(),
        parent_prompt_id: None,
        child_session_id: acp::SessionId::new(manager_id),
        subagent_type: "manager".into(),
        persona: None,
        description: "coordinate work".into(),
        started_at: std::time::Instant::now(),
        child_handle: manager_handle,
        child_thread: crate::session::SessionThread::from_handle(std::thread::spawn(|| {})),
        cancel_token: tokio_util::sync::CancellationToken::new(),
        resumed_from: None,
        child_cwd: "/tmp/manager-cwd".into(),
        worktree_path: None,
        effective_model_id: "test-model".into(),
        run_in_background: false,
        surface_completion: true,
        color: None,
        block_waited: false,
        explicitly_killed: false,
    });

    assert!(
        agent
            .sessions
            .borrow()
            .get(&acp::SessionId::new(manager_id))
            .is_none(),
        "manager subagent must not be in MvpAgent.sessions (precondition)"
    );

    let ctx = agent
        .try_build_subagent_spawn_context(manager_id)
        .expect("nested parent (manager subagent) must resolve via coordinator");

    assert_eq!(ctx.parent_session_id, manager_id);
    assert_eq!(ctx.parent_depth, 1, "worker should inherit manager depth 1");
    assert_eq!(
        ctx.parent_cwd,
        std::path::PathBuf::from("/tmp/manager-cwd")
    );
    assert_eq!(ctx.parent_agent_name.as_deref(), Some("manager"));
    // Shared gate proves we used the coordinator's SessionHandle, not a default.
    use std::sync::atomic::Ordering::Relaxed;
    parent_gate.store(true, Relaxed);
    assert!(
        ctx.goal_loop_active.load(Relaxed),
        "spawn context must share the nested parent's goal-loop gate"
    );
}
