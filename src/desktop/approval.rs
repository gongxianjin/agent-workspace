use agentranch::core::service::{ApprovalView, UiCommand};

pub fn decision_command(approval: &ApprovalView, decision_id: &str, now: u64) -> Option<UiCommand> {
    if !approval.can_decide || now >= approval.expires_at || !approval.offered.iter().any(|d| d.id == decision_id) {
        return None;
    }
    Some(UiCommand::Decide {
        run_id: approval.run_id.clone(), attempt: approval.attempt, session: approval.session.clone(),
        request_id: approval.request_id.clone(), generation: approval.generation,
        scope_hash: approval.scope_hash.clone(), decision_id: decision_id.into(),
    })
}
