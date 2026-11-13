#[path = "../src/desktop/approval.rs"]
mod approval;
use agentranch::core::service::{ApprovalView, OfferedDecision, DecisionKind, UiCommand};

fn offer() -> ApprovalView {
    ApprovalView {
        run_id: "run-one".into(), attempt: 2, session: "session-two".into(), request_id: "request-3".into(),
        generation: 4, scope_hash: "scope-5".into(), expires_at: 200,
        offered: vec![OfferedDecision { id: "specific-allow".into(), kind: DecisionKind::AllowOnce, label: "Allow once".into() }],
        summary: "Controlled approval".into(), simulated: true, can_decide: true,
    }
}
#[test]
fn approval_submission_preserves_every_current_execution_binding() {
    match approval::decision_command(&offer(), "specific-allow", 100).unwrap() {
        UiCommand::Decide { run_id, attempt, session, request_id, generation, scope_hash, decision_id } => {
            assert_eq!((run_id.as_str(), attempt, session.as_str()), ("run-one", 2, "session-two"));
            assert_eq!((request_id.as_str(), generation, scope_hash.as_str(), decision_id.as_str()), ("request-3", 4, "scope-5", "specific-allow"));
        }
        other => panic!("wrong command: {other:?}"),
    }
}
#[test]
fn revoked_expired_and_not_offered_decisions_produce_no_command() {
    let mut p = offer();
    assert!(approval::decision_command(&p, "specific-allow", 200).is_none());
    assert!(approval::decision_command(&p, "specific-allow", 201).is_none());
    assert!(approval::decision_command(&p, "allow_once", 100).is_none());
    p.can_decide = false;
    assert!(approval::decision_command(&p, "specific-allow", 100).is_none());
}
#[test]
fn updated_snapshot_is_used_instead_of_old_approval_context() {
    let mut p = offer();
    p.session = "new-session".into(); p.attempt = 3; p.generation = 6;
    p.offered[0].id = "new-offer".into();
    assert!(approval::decision_command(&p, "specific-allow", 100).is_none());
    match approval::decision_command(&p, "new-offer", 100).unwrap() {
        UiCommand::Decide { attempt, session, generation, .. } => assert_eq!((attempt, session.as_str(), generation), (3, "new-session", 6)),
        _ => panic!("wrong command"),
    }
}
