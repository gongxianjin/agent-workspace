// UI interaction state only; no alternate task state machine or mocked service.
#[path = "../src/desktop/pending.rs"]
mod pending;
use pending::Pending;

#[test]
fn accepted_action_disables_only_its_own_duplicate_until_its_result() {
    let mut pending = Pending::default();
    pending.accept("cancel:run-a".into(), 17);
    assert!(pending.contains("cancel:run-a"));
    assert!(!pending.contains("cancel:run-b"));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending.complete(17).as_deref(), Some("cancel:run-a"));
    assert!(!pending.contains("cancel:run-a"));
}

#[test]
fn out_of_order_and_unknown_results_do_not_release_another_action() {
    let mut pending = Pending::default();
    pending.accept("report:run-a".into(), 101);
    pending.accept("approvals:run-b".into(), 102);
    assert_eq!(pending.complete(999), None);
    assert_eq!(pending.len(), 2);
    assert_eq!(pending.complete(102).as_deref(), Some("approvals:run-b"));
    assert!(pending.contains("report:run-a"));
    assert_eq!(pending.complete(102), None);
    assert_eq!(pending.complete(101).as_deref(), Some("report:run-a"));
    assert_eq!(pending.len(), 0);
}

#[test]
fn different_actions_on_the_same_run_remain_independently_pending() {
    let mut pending = Pending::default();
    pending.accept("status:run-a".into(), 4);
    pending.accept("cancel:run-a".into(), 5);
    assert_eq!(pending.complete(4).as_deref(), Some("status:run-a"));
    assert!(pending.contains("cancel:run-a"));
    assert_eq!(pending.len(), 1);
}
