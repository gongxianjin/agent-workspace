//! Wave 8A AO ownership boundary tests.
//! The HTTP fixture is local-only and does not represent AO or Linux acceptance.
//! An AO report, even with termination-looking fields, is not independent wait proof.

use agentranch::{
    adapters::{ao_http::AoHttp, AoPort, Observation},
    config::AoConfig,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

fn evidence_root(case_name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("dist")
        .join(format!(
            "ao-wave8a-observer-contract-20261108-{case_name}-{}",
            uuid::Uuid::new_v4()
        ));
    fs::create_dir_all(&root).unwrap();
    println!("AO_WAVE8A_EVIDENCE={}", root.display());
    root
}

fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut raw = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).unwrap();
        raw.push(byte[0]);
        assert!(raw.len() < 16 * 1024, "fixture request header bound");
        if raw.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header_end = raw.len();
    let header = String::from_utf8(raw.clone()).unwrap();
    let content_length = header.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(|value| value.trim().parse::<usize>().unwrap())
    });
    if let Some(length) = content_length {
        assert!(length <= 64 * 1024, "fixture response request body bound");
        let mut body = vec![0_u8; length];
        stream.read_exact(&mut body).unwrap();
        raw.extend_from_slice(&body);
    }
    assert!(header_end <= raw.len());
    raw
}

fn response(body: Value) -> String {
    let body = body.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn routes(root: &Path) -> Vec<String> {
    serde_json::from_slice(&fs::read(root.join("routes.json")).unwrap()).unwrap()
}

fn observe_with_fixture(
    root: &Path,
    session: &str,
    report: Option<Value>,
    session_value: Value,
) -> Observation {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let evidence = root.to_owned();
    let session = session.to_owned();
    let server_session = session.clone();
    let expected_requests = if report.is_some() { 2 } else { 3 };
    let report_value = report.unwrap_or_else(|| json!({"reports": []}));
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        let mut routes = Vec::new();
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().unwrap();
            let raw = read_request(&mut stream);
            let request_line = String::from_utf8_lossy(&raw)
                .lines()
                .next()
                .unwrap()
                .to_owned();
            let path = request_line.split_whitespace().nth(1).unwrap().to_owned();
            let body = if path.starts_with("/api/v1/sessions/") && path.ends_with("/conversation?limit=200") {
                json!({"sessionId": server_session.as_str(), "activities": []})
            } else if path.starts_with("/api/v1/reports?") {
                report_value.clone()
            } else if path == format!("/api/v1/sessions/{server_session}") {
                session_value.clone()
            } else {
                panic!("unexpected AoHttp route: {path}");
            };
            routes.push(path);
            requests.push(raw);
            stream.write_all(response(body).as_bytes()).unwrap();
        }
        let raw = requests
            .iter()
            .flat_map(|request| request.iter().copied().chain([b'\n']))
            .collect::<Vec<_>>();
        fs::write(evidence.join("requests.raw.bin"), raw).unwrap();
        fs::write(
            evidence.join("routes.json"),
            serde_json::to_vec_pretty(&routes).unwrap(),
        )
        .unwrap();
    });

    let config = AoConfig {
        base_url: format!("http://{address}"),
        project_id: "owned-project".into(),
        model: "fixture-model".into(),
    };
    let mut adapter = AoHttp::new(config).unwrap();
    let observation = adapter.observe(&session).unwrap();
    server.join().unwrap();
    fs::write(
        root.join("observation.debug.txt"),
        format!("{observation:?}\n"),
    )
    .unwrap();
    observation
}

#[test]
fn done_ao_report_with_exit_like_fields_remains_reported_without_proof() {
    let root = evidence_root("done-report");
    let session = "ao-worker-1";
    let observation = observe_with_fixture(
        &root,
        session,
        Some(json!({
            "reports": [{
                "id": "ao-report-1",
                "sessionId": session,
                "state": "done",
                "status": "terminated",
                "isTerminated": true,
                "exit_code": 0,
                "pid": 4242,
                "wait_completed": true,
                "native_exit_proof": {"empty": true, "observer_alive": true},
                "reason_receipt": {"id": "receipt-1", "state": "delivered"},
                "delivery": {"state": "delivered", "receipt_id": "receipt-1"},
                "evidence": {"source": "ao-receipt", "synthetic": true}
            }]
        })),
        json!({
            "session": {"id": session, "isTerminated": true, "provisionState": "ready"}
        }),
    );

    let Observation::Reported { report, verification } = observation else {
        panic!("an AO report must remain Reported, never become ExitProof");
    };
    assert_eq!(report["exit_code"], 0);
    assert_eq!(report["pid"], 4242);
    assert_eq!(report["native_exit_proof"]["empty"], true);
    assert_eq!(report["reason_receipt"]["state"], "delivered");
    assert_eq!(report["delivery"]["state"], "delivered");
    assert!(verification["passed"].is_null());
    assert_eq!(verification["state"], "unknown");
    assert!(verification["reason"].as_str().unwrap().contains("not proof"));
}

#[test]
fn terminated_ao_session_without_report_remains_unknown_and_uses_no_fixture_fallback() {
    let root = evidence_root("terminated-without-report");
    let session = "ao-worker-2";
    let observation = observe_with_fixture(
        &root,
        session,
        None,
        json!({
            "session": {
                "id": session,
                "isTerminated": true,
                "provisionState": "failed",
                "exit_code": 0,
                "pid": 5252,
                "status": "terminated"
            }
        }),
    );

    let Observation::Unknown(reason) = observation else {
        panic!("terminated AO session without independent wait evidence must remain Unknown");
    };
    assert!(reason.contains("no independently observed process exit"));
    assert_eq!(
        routes(&root),
        vec![
            format!("/api/v1/sessions/{session}/conversation?limit=200"),
            format!("/api/v1/reports?projectId=owned-project"),
            format!("/api/v1/sessions/{session}"),
        ],
        "real AoHttp observation must use only AO HTTP routes and never a local fixture path",
    );
}
