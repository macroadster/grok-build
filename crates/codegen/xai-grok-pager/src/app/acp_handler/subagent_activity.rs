use super::*;

/// Update the activity label on a subagent's collapsed scrollback block.
///
/// Skips the write (and cache invalidation) when the label hasn't changed,
/// so the per-delta common case ("Responding" stays "Responding") allocates
/// nothing.
pub(super) fn sync_activity_label(
    scrollback: &mut crate::scrollback::state::ScrollbackState,
    entry_id: Option<crate::scrollback::entry::EntryId>,
    activity_label: Option<&str>,
) {
    if let Some(eid) = entry_id
        && let Some(entry) = scrollback.get_by_id_mut(eid)
        && let RenderBlock::Subagent(ref mut sb) = entry.block
        && sb.activity_label.as_deref() != activity_label
    {
        sb.activity_label = activity_label.map(str::to_owned);
        entry.invalidate_cache();
    }
}

/// Fan a subagent's computed activity label out to both surfaces that show
/// it — the collapsed scrollback block and the [`SubagentInfo`] backing the
/// tasks pane / dashboard rows — so the two can't drift.
///
/// Searches nested intermediate parents so a worker under a manager still
/// updates the manager's tasks-pane row (entrepreneur → manager → worker).
pub(super) fn sync_subagent_activity(
    parent: &mut AgentView,
    child_key: &str,
    activity_label: Option<String>,
) {
    if parent.subagent_sessions.contains_key(child_key) {
        let info = parent.subagent_sessions.get_mut(child_key).expect("checked");
        sync_activity_label(
            &mut parent.scrollback,
            info.scrollback_entry_id,
            activity_label.as_deref(),
        );
        info.activity_label = activity_label;
        return;
    }
    let next_key = parent
        .subagent_views
        .iter()
        .find(|(_, child)| super::routing::subagent_tree_contains(child, child_key))
        .map(|(k, _)| k.clone());
    if let Some(k) = next_key
        && let Some(child) = parent.subagent_views.get_mut(&k)
    {
        sync_subagent_activity(child, child_key, activity_label);
    }
}

/// Resolve a subagent child view's live activity into the display label the
/// fan-out stamps ("Waiting" while the child is busy between activities).
pub(super) fn subagent_activity_label(child_view: &AgentView) -> Option<String> {
    match child_view.resolve_turn_activity() {
        Some(a) => Some(crate::app::subagent::format_activity_label(&a)),
        None if child_view.session.state.is_busy() => Some("Waiting".to_string()),
        None => None,
    }
}

/// Synthesize a finish for a stuck row when a kill found nothing live to stop
/// (else `pending_kill` times out → "running"). `status` is the real terminal
/// status for an already-finished orphan, else `"cancelled"`.
/// Find an unfinished subagent by id anywhere in the tree (direct or nested).
fn find_unfinished_child_session_id(view: &AgentView, subagent_id: &str) -> Option<String> {
    if let Some(id) = view
        .subagent_sessions
        .values()
        .find(|i| i.subagent_id.as_ref() == subagent_id && !i.finished)
        .map(|i| i.child_session_id.to_string())
    {
        return Some(id);
    }
    for child in view.subagent_views.values() {
        if let Some(id) = find_unfinished_child_session_id(child, subagent_id) {
            return Some(id);
        }
    }
    None
}

pub(crate) fn finalize_killed_subagent(
    app: &mut AppView,
    session_id: &acp::SessionId,
    subagent_id: &str,
    status: &str,
) -> bool {
    // Parent may be root (entrepreneur/manager top-level) or a nested
    // intermediate (manager under entrepreneur killing a worker).
    let Some(matched) = find_session_match(app, session_id) else {
        return false;
    };
    let agent_id = matched.agent_id();
    let Some(agent) = app.agents.get(&agent_id) else {
        return false;
    };
    // Idempotency: skip if already finished. Search nested so manager→worker
    // kills resolve when the kill is issued against the manager session.
    let parent_view: &AgentView = match matched {
        SessionMatch::Root(_) => agent,
        SessionMatch::Child(_) => {
            let child_sid = session_id.0.as_ref();
            // Immutable recursive walk (mirror of find_subagent_view_mut).
            fn find_imm<'a>(view: &'a AgentView, sid: &str) -> Option<&'a AgentView> {
                if let Some(c) = view.subagent_views.get(sid) {
                    return Some(c);
                }
                for child in view.subagent_views.values() {
                    if let Some(found) = find_imm(child, sid) {
                        return Some(found);
                    }
                }
                None
            }
            match find_imm(agent, child_sid) {
                Some(v) => v,
                None => return false,
            }
        }
    };
    let Some(child_session_id) = find_unfinished_child_session_id(parent_view, subagent_id) else {
        return false;
    };

    let payload = SessionNotification {
        session_id: session_id.clone(),
        update: XaiSessionUpdate::SubagentFinished {
            subagent_id: subagent_id.to_string(),
            child_session_id,
            // An already-finished orphan may be "failed", but the cancel response
            // carries no failure reason (lost across the resume window), so
            // `error` stays None.
            status: status.to_string(),
            error: None,
            tool_calls: 0,
            turns: 0,
            // Real run time is unknown for an already-gone orphan (the row's
            // started_at is stamped at resume, not the real spawn), so emit 0.
            duration_ms: 0,
            tokens_used: 0,
            output: None,
            will_wake: false,
        },
        meta: None,
    };
    let Ok(params) = serde_json::value::to_raw_value(&payload) else {
        return false;
    };
    let notif = acp::ExtNotification::new("x.ai/session/update", params.into());
    handle_ext_notification(&notif, app)
}
