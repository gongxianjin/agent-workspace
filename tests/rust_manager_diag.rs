//! manager-diag.log contract: manager-local diagnostic lines under --state-dir,
//! key events only, no secrets. Each line: `<unix_ts>\t<event>\t<detail>`.
use ao_reason_workbench::core::service::append_diag;
use std::path::Path;

fn temp_dir(tag:&str)->std::path::PathBuf {
 let dir=std::env::temp_dir().join(format!("manager-diag-{tag}-{}",std::process::id()));
 let _=std::fs::remove_dir_all(&dir);std::fs::create_dir_all(&dir).unwrap();dir
}

#[test]
fn diag_lines_are_tsv_and_ordered() {
 let dir=temp_dir("tsv");
 append_diag(&dir,"gateway_configured","worker=B host=172.28.8.221").unwrap();
 append_diag(&dir,"dispatch_result","run_id=abc status=done").unwrap();
 let text=std::fs::read_to_string(dir.join("manager-diag.log")).unwrap();
 let lines:Vec<&str>=text.lines().collect();
 assert_eq!(lines.len(),2);
 for line in &lines {
  let parts:Vec<&str>=line.split('\t').collect();
  assert_eq!(parts.len(),3,"line must be unix_ts\\tevent\\tdetail: {line}");
  assert!(parts[0].chars().all(|c|c.is_ascii_digit()),"timestamp must be ascii digits: {}",parts[0]);
 }
 assert!(lines[0].ends_with("gateway_configured\tworker=B host=172.28.8.221"));
 assert!(lines[1].ends_with("dispatch_result\trun_id=abc status=done"));
}

#[test]
fn diag_rejects_tabs_and_newlines_in_event_and_detail() {
 let dir=temp_dir("inject");
 assert!(append_diag(&dir,"bad\tevent","x").is_err(),"tab in event breaks the 3-field shape");
 assert!(append_diag(&dir,"ok","bad\ndetail").is_err(),"newline in detail forges extra lines");
 assert!(!dir.join("manager-diag.log").exists(),"rejected writes must not create the log");
}

#[test]
fn diag_appends_across_calls_and_is_ascii() {
 let dir=temp_dir("append");
 for i in 0..3 { append_diag(&dir,"tick",&format!("i={i}")).unwrap(); }
 let text=std::fs::read_to_string(dir.join("manager-diag.log")).unwrap();
 assert_eq!(text.lines().count(),3);
 assert!(text.is_ascii(),"diagnostic log stays ascii; callers pass ids and status only");
 let _=Path::new(&dir);
}

#[test]
fn diag_creates_missing_state_dir_instead_of_dropping_lines() {
 // Regression: in real-mode sessions the manager never creates --state-dir at
 // startup (projects/Projects::open is read-only; only materialize_artifact_
 // package create_dir_all's it after a done run). The first two diag lines
 // (gateway_configured, dispatch_result) were silently dropped by the `let _ =`
 // at every call site — evidence: -41/-43 manager-diag.log has exactly 1 line.
 let base=tempfile::tempdir().unwrap();
 let missing=base.path().join("fresh-state");
 append_diag(&missing,"gateway_configured","worker=B artifact=9138e82e")
  .expect("append_diag must create the state dir rather than drop the line");
 let text=std::fs::read_to_string(missing.join("manager-diag.log")).unwrap();
 assert_eq!(text.lines().count(),1,"first diag line survives even when state_dir does not exist yet");
 let line=text.trim_end_matches('\n');
 assert!(line.ends_with("gateway_configured\tworker=B artifact=9138e82e"),"\u{2014} actual: {line}");
}
