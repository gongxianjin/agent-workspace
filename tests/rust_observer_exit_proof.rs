use ao_reason_workbench::{
    adapters::{AoPort, Observation, WorkRequest},
    config::{Actor, NodeSpec},
    core::{runner::Runner, store::{ExitProof, Store}},
};
use serde_json::json;

fn node() -> NodeSpec {
    NodeSpec {
        id: "B".into(),
        address: "observer-contract-B/runner".into(),
        key_hex: "07".repeat(32),
        slots: 1,
        admitted: true,
        healthy: true,
        draining: false,
        templates: vec!["sum-native-v1".into()],
    }
}

struct ScriptedObserver {
    session: String,
    observation: Option<Observation>,
}

impl AoPort for ScriptedObserver {
    fn create(&mut self, _request: &WorkRequest) -> anyhow::Result<String> {
        Ok(self.session.clone())
    }

    fn observe(&mut self, session: &str) -> anyhow::Result<Observation> {
        assert_eq!(session, self.session);
        Ok(self.observation.take().unwrap_or(Observation::Unknown(
            "observer fixture exhausted".into(),
        )))
    }

    fn resolve(&mut self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("approval is not part of this fixture")
    }

    fn kill(&mut self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("cancellation is not part of this fixture")
    }
}

struct CancelPathObserver {
    session: String,
    observations: Vec<Observation>,
}

impl AoPort for CancelPathObserver {
    fn create(&mut self, _request: &WorkRequest) -> anyhow::Result<String> {
        Ok(self.session.clone())
    }

    fn observe(&mut self, session: &str) -> anyhow::Result<Observation> {
        assert_eq!(session, self.session);
        Ok(self.observations.remove(0))
    }

    fn resolve(&mut self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("approval is not part of this fixture")
    }

    fn kill(&mut self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn exit_proof(run_id: &str, attempt: u32, epoch: &str, session: Option<&str>) -> ExitProof {
    ExitProof {
        run_id: run_id.into(),
        attempt,
        epoch: epoch.into(),
        session: session.map(str::to_owned),
        empty: true,
        observer_alive: true,
        not_started: false,
        owner_evidence: None,
    }
}

fn setup() -> (tempfile::TempDir, Store, Runner, String) {
    let temp = tempfile::tempdir().unwrap();
    let node = node();
    let mut store = Store::open(&temp.path().join("controller.sqlite"), "observer-contract", &[node.clone()]).unwrap();
    let run = store.dispatch(&Actor::Local("observer-owner".into()), "sum-native-v1", 100).unwrap();
    store.assign_next(100).unwrap().unwrap();
    let mut runner = Runner::open(&temp.path().join("runner.sqlite"), "observer-contract", node).unwrap();
    for (_, message) in store.pending_outbox(8).unwrap() {
        runner.accept(&message, 100).unwrap();
    }
    (temp, store, runner, run.id)
}

#[test]
fn reported_observation_retains_slot_without_exit_proof() {
    let (_temp, mut store, mut runner, run_id) = setup();
    let mut observer = ScriptedObserver {
        session: "unused".into(),
        observation: Some(Observation::Reported {
            report: json!({"schema":"observer-report/1"}),
            verification: json!({"state":"report_pending_exit","passed":null}),
        }),
    };
    runner.tick(&mut observer, 100).unwrap();
    let events = runner.pending_outbox(8).unwrap();
    let event = events.iter().find(|event| event.kind == "report").unwrap();
    assert_eq!(event.payload["verification"]["state"], "report_pending_exit");
    for event in events {
        store.receive(&event, 100).unwrap();
    }
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
}

#[test]
fn generic_completed_with_successful_verification_retains_unknown_reservation() {
    let (_temp, mut store, mut runner, run_id) = setup();
    let mut observer = ScriptedObserver {
        session: "unused".into(),
        observation: Some(Observation::Completed {
            report: json!({"schema":"observer-report/1", "sum":6}),
            verification: json!({"passed":true, "source":"generic-provider"}),
        }),
    };

    runner.tick(&mut observer, 100).unwrap();
    let events = runner.pending_outbox(8).unwrap();
    let report = events.iter().find(|event| event.kind == "report").unwrap();
    assert!(report.payload["exit_proof"].is_null(), "generic completion must not enter the trusted envelope");
    assert!(report.payload["verification"]["passed"].is_null(), "provider verification must not project terminal success");
    assert_eq!(report.payload["verification"]["state"], "exit_proof_required");
    for event in events {
        store.receive(&event, 100).unwrap();
    }
    assert_eq!(store.run(&run_id).unwrap().status, "unknown");
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
}

#[test]
fn matching_observed_minimal_exit_proof_retains_assignment_without_m0_evidence() {
    let (_temp, mut store, mut runner, run_id) = setup();
    let session = "observer-session";
    let epoch = store.run(&run_id).unwrap().epoch;
    let proof = exit_proof(&run_id, 1, &epoch, Some(session));
    let mut observer = ScriptedObserver {
        session: session.into(),
        observation: Some(Observation::ExitProof {
            proof,
            report: json!({"schema":"observer-report/1"}),
            verification: json!({"passed":true}),
        }),
    };
    runner.tick(&mut observer, 100).unwrap();
    let events = runner.pending_outbox(8).unwrap();
    let event = events.iter().find(|event| event.kind == "report").unwrap();
    assert!(event.payload.get("exit_proof").is_none_or(serde_json::Value::is_null));
    assert_eq!(event.payload["verification"]["state"], "owner_evidence_required");
    for event in events {
        store.receive(&event, 100).unwrap();
    }
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
}

#[test]
fn mismatched_observed_exit_identity_does_not_release_assignment() {
    for (attempt, epoch, session) in [
        (2, "observer-epoch", "observer-session"),
        (1, "wrong-epoch", "observer-session"),
        (1, "observer-epoch", "wrong-session"),
    ] {
        let (_temp, mut store, mut runner, run_id) = setup();
        let expected_epoch = store.run(&run_id).unwrap().epoch;
        let proof = exit_proof(&run_id, attempt, epoch, Some(session));
        let mut observer = ScriptedObserver {
            session: "observer-session".into(),
            observation: Some(Observation::ExitProof {
                proof,
                report: json!({"schema":"observer-report/1"}),
                verification: json!({"passed":true}),
            }),
        };
        runner.tick(&mut observer, 100).unwrap();
        let events = runner.pending_outbox(8).unwrap();
        let event = events.iter().find(|event| event.kind == "report").unwrap();
        assert!(event.payload.get("exit_proof").is_none(), "mismatch must not enter the trusted envelope");
        assert_eq!(event.payload["verification"]["state"], "exit_proof_rejected");
        for event in events {
            store.receive(&event, 100).unwrap();
        }
        assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released,
            "mismatch must retain assignment: attempt={attempt} epoch={epoch} session={session} expected_epoch={expected_epoch}");
    }
}

#[test]
fn cancel_path_generic_exited_retains_unknown_reservation_without_exit_proof() {
    let (_temp, mut store, mut runner, run_id) = setup();
    let mut start_observer = ScriptedObserver {
        session: "cancel-session".into(),
        observation: Some(Observation::Running),
    };
    runner.tick(&mut start_observer, 100).unwrap();
    for (_, event) in store.pending_outbox(8).unwrap() {
        runner.accept(&event, 100).unwrap();
    }
    for event in runner.pending_outbox(8).unwrap() {
        store.receive(&event, 100).unwrap();
        runner.acknowledge_outbox(&event.message_id).unwrap();
    }

    store.expire(1000).unwrap();
    let cancel = store.pending_outbox(8).unwrap().into_iter().find_map(|(_, event)| {
        (event.kind == "cancel").then_some(event)
    }).unwrap();
    runner.accept(&cancel, 1000).unwrap();

    let mut cancel_observer = CancelPathObserver {
        session: "cancel-session".into(),
        observations: vec![Observation::Exited],
    };
    runner.tick(&mut cancel_observer, 1000).unwrap();
    let events = runner.pending_outbox(8).unwrap();
    let status = events.iter().find(|event| event.kind == "status").unwrap();
    assert!(status.payload["exit_proof"].is_null(), "generic cancellation exit must not enter the trusted envelope");
    assert_eq!(status.payload["state"], "unknown");
    for event in events {
        store.receive(&event, 1000).unwrap();
    }
    assert_eq!(store.run(&run_id).unwrap().status, "cancel_pending");
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
}

#[test]
fn unknown_observation_retains_reservation_and_does_not_reassign() {
    let (_temp, mut store, mut runner, run_id) = setup();
    let mut observer = ScriptedObserver {
        session: "unused".into(),
        observation: Some(Observation::Unknown("observer identity unavailable".into())),
    };
    runner.tick(&mut observer, 100).unwrap();
    let event = runner.pending_outbox(8).unwrap().pop().unwrap();
    store.receive(&event, 100).unwrap();
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
    assert!(store.assign_next(101).unwrap().is_none());
}

