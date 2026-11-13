use anyhow::Result;
use ao_reason_workbench::core::{protocol::{Envelope, PROTOCOL}, relay::{RelayBridge, RelayEndpoint, RelayOutcome, MAX_RELAY_FRAME}};
use serde_json::json;

struct RecordingEndpoint {
    frames: Vec<Vec<u8>>,
}

impl RelayEndpoint for RecordingEndpoint {
    fn deliver(&mut self, frame: &[u8]) -> Result<()> {
        self.frames.push(frame.to_vec());
        Ok(())
    }
}

fn signed_envelope(to: &str, deployment: &str) -> Envelope {
    let mut message = Envelope {
        schema: PROTOCOL.into(),
        message_id: "relay-message-1".into(),
        deployment: deployment.into(),
        from: "A".into(),
        to: to.into(),
        run_id: "relay-run-1".into(),
        attempt: 1,
        epoch: "relay-epoch-1".into(),
        kind: "start".into(),
        sent_at: 100,
        expires_at: 400,
        payload: json!({"template":"sum-native-v1","bytes":"preserve"}),
        mac: String::new(),
    };
    message.sign(&[7; 32]).unwrap();
    message
}

#[test]
fn signed_frame_is_forwarded_byte_for_byte_to_the_recipient_endpoint() {
    let message = serde_json::to_vec(&signed_envelope("B", "relay-deployment")).unwrap();
    let frame = [b"  \n".as_slice(), &message, b"\n  ".as_slice()].concat();
    let mut bridge = RelayBridge::new("relay-deployment", "B", [7; 32]).unwrap();
    let mut recipient = RecordingEndpoint { frames: Vec::new() };

    let outcome = bridge.forward(&frame, &mut recipient, 110).unwrap();

    assert_eq!(outcome, RelayOutcome::Forwarded);
    assert_eq!(recipient.frames, vec![frame]);
}

#[test]
fn deployment_or_recipient_mismatch_is_rejected_before_forwarding() {
    for (deployment, recipient) in [("other-deployment", "B"), ("relay-deployment", "C")] {
        let frame = serde_json::to_vec(&signed_envelope(recipient, deployment)).unwrap();
        let mut bridge = RelayBridge::new("relay-deployment", "B", [7; 32]).unwrap();
        let mut endpoint = RecordingEndpoint { frames: Vec::new() };

        assert!(bridge.forward(&frame, &mut endpoint, 110).is_err());
        assert!(endpoint.frames.is_empty());
    }
}

#[test]
fn duplicate_message_id_is_accepted_without_duplicate_delivery() {
    let frame = serde_json::to_vec(&signed_envelope("B", "relay-deployment")).unwrap();
    let mut bridge = RelayBridge::new("relay-deployment", "B", [7; 32]).unwrap();
    let mut recipient = RecordingEndpoint { frames: Vec::new() };

    assert_eq!(bridge.forward(&frame, &mut recipient, 110).unwrap(), RelayOutcome::Forwarded);
    assert_eq!(bridge.forward(&frame, &mut recipient, 110).unwrap(), RelayOutcome::Duplicate);
    assert_eq!(recipient.frames, vec![frame]);
}

#[test]
fn frame_larger_than_the_relay_bound_is_rejected_without_delivery() {
    let frame = vec![b' '; MAX_RELAY_FRAME + 1];
    let mut bridge = RelayBridge::new("relay-deployment", "B", [7; 32]).unwrap();
    let mut recipient = RecordingEndpoint { frames: Vec::new() };

    assert!(bridge.forward(&frame, &mut recipient, 110).is_err());
    assert!(recipient.frames.is_empty());
}
