// Breaks caught: float/oversize wires, unaudited schema, partial transactions, float load choice.
use ao_reason_workbench::{config::{Actor,NodeSpec},core::{protocol::{Envelope,PROTOCOL,canonical},store::Store}};
use serde_json::json;
fn node(id:&str,slots:u32)->NodeSpec{NodeSpec{id:id.into(),address:format!("local-{id}/runner"),key_hex:if id=="B"{"07"}else{"08"}.repeat(32),slots,admitted:true,healthy:true,draining:false,templates:vec!["sum-native-v1".into(),"control-native-v1".into()]}}
#[test]
fn protocol_refuses_float_and_excessive_nesting_or_size(){
 assert!(canonical(&json!({"value":1.5})).is_err());let mut nested=json!({});for _ in 0..34{nested=json!({"nested":nested});}assert!(canonical(&nested).is_err());
 let mut m=Envelope{schema:PROTOCOL.into(),message_id:"m".into(),deployment:"native".into(),from:"A".into(),to:"B".into(),run_id:"r".into(),attempt:1,epoch:"e".into(),kind:"start".into(),sent_at:1,expires_at:301,payload:json!({"v":"x".repeat(66000)}),mac:String::new()};m.sign(&[7;32]).unwrap();assert!(m.verify(&[7;32],"native","B",2).is_err());
}
#[test]
fn scheduler_uses_exact_fraction_then_stable_id(){let t=tempfile::tempdir().unwrap();let mut s=Store::open(&t.path().join("native.db"),"native",&[node("C",6),node("B",3)]).unwrap();for _ in 0..3{s.dispatch(&Actor::Local("me".into()),"sum-native-v1",1).unwrap();}assert_eq!(s.assign_next(2).unwrap().unwrap().node_id,"B");assert_eq!(s.assign_next(2).unwrap().unwrap().node_id,"C");assert_eq!(s.assign_next(2).unwrap().unwrap().node_id,"C");}
#[test]
fn unsupported_fifo_head_cannot_be_skipped(){let t=tempfile::tempdir().unwrap();let mut n=node("B",2);n.templates=vec!["sum-native-v1".into()];let mut s=Store::open(&t.path().join("native.db"),"native",&[n]).unwrap();let head=s.dispatch(&Actor::Local("me".into()),"control-native-v1",1).unwrap();s.dispatch(&Actor::Local("me".into()),"sum-native-v1",1).unwrap();assert!(s.assign_next(2).unwrap().is_none());s.cancel(&Actor::Local("me".into()),&head.id).unwrap();assert!(s.assign_next(2).unwrap().is_some());}
#[test]
fn assignment_and_start_outbox_rollback_together_on_storage_failure(){let t=tempfile::tempdir().unwrap();let path=t.path().join("native.db");let mut s=Store::open(&path,"native",&[node("B",1)]).unwrap();let r=s.dispatch(&Actor::Local("me".into()),"sum-native-v1",1).unwrap();let db=rusqlite::Connection::open(&path).unwrap();db.execute_batch("CREATE TRIGGER owned_fault BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT,'owned simulated disk failure'); END;").unwrap();assert!(s.assign_next(2).is_err());assert!(s.assignment(&r.id,1).unwrap().is_none());assert_eq!(s.run(&r.id).unwrap().status,"queued");assert_eq!(s.outbox_count().unwrap(),0);db.execute_batch("DROP TRIGGER owned_fault;").unwrap();assert!(s.assign_next(2).unwrap().is_some());}
#[test]
fn future_schema_is_refused_without_downgrade_and_legacy_is_not_imported(){let t=tempfile::tempdir().unwrap();let path=t.path().join("future.db");let db=rusqlite::Connection::open(&path).unwrap();db.pragma_update(None,"user_version",99).unwrap();assert!(Store::open(&path,"native",&[]).is_err());assert_eq!(db.pragma_query_value(None,"user_version",|r|r.get::<_,i64>(0)).unwrap(),99);let path=t.path().join("legacy.db");let db=rusqlite::Connection::open(&path).unwrap();db.execute_batch("CREATE TABLE old_ledger(secret TEXT);").unwrap();assert!(Store::open(&path,"native",&[]).is_err());}
#[test]
fn sqlite_persists_wal_journal_mode(){let t=tempfile::tempdir().unwrap();let path=t.path().join("native.db");let _s=Store::open(&path,"native",&[]).unwrap();let db=rusqlite::Connection::open(&path).unwrap();let journal:String=db.pragma_query_value(None,"journal_mode",|r|r.get(0)).unwrap();assert_eq!(journal,"wal");}
