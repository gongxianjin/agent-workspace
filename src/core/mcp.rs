//! Line-framed stdio MCP. Actor is injected by the trusted CLI host, never tool JSON.
use std::io::{BufRead,Read,Write};
use anyhow::{Result,ensure};
use serde_json::{Value,json};
use super::controller::ControlService;
pub fn tools(local:bool)->Value {
 let templates=if local{json!(["sum-native-v1","control-native-v1"])}else{json!(["sum-v1","control-v1"])};
 let string=json!({"type":"string","minLength":1,"maxLength":128});let mut tools=Vec::new();
 for name in ControlService::tool_names(){let(properties,required)=match name {
  "dispatch"=>(json!({"template":{"type":"string","enum":templates}}),json!(["template"])),
  "retry"=>(json!({"run_id":string,"attempt":{"type":"integer","minimum":1}}),json!(["run_id","attempt"])),
  "decide"=>(json!({"run_id":string,"request_id":string,"generation":{"type":"integer","minimum":1},"scope_hash":string,"decision_id":{"type":"string","enum":["allow_once","reject_once"]}}),json!(["run_id","request_id","generation","scope_hash","decision_id"])),
  _=>(json!({"run_id":string}),json!(["run_id"])),
 };tools.push(json!({"name":name,"description":match name{"dispatch"=>"Persist one fixed-template task; queued is not completed","status"=>"Read owned durable run state","approvals"=>"Read current exact offered once-only approvals","decide"=>"Persist exact decision intent; not proof of effect","report"=>"Read actual report and independent verification","retry"=>"Retry failed/cancelled execution only with exact old exit proof",_=>"Request cancellation; not proof of process exit"},"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}));}
 json!({"tools":tools})
}
fn rpc_error(id:Value,code:i64,message:&str)->Value{json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})}
pub fn handle(request:Value,controller:&mut Option<ControlService>,local:bool,now:u64)->Option<Value>{
 let Some(object)=request.as_object()else{return Some(rpc_error(Value::Null,-32600,"Invalid request"))};
 let id=object.get("id").cloned();
 if request["jsonrpc"]!="2.0"||!request["method"].is_string(){return Some(rpc_error(id.unwrap_or(Value::Null),-32600,"Invalid request"))}
 let id=id?;
 let result=match request["method"].as_str().unwrap_or("") {
  "initialize"=>{let supported=["2025-11-25","2025-06-18","2025-03-26","2024-11-05"];let version=request["params"]["protocolVersion"].as_str().filter(|s|supported.contains(s)).unwrap_or(supported[0]);json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"ao-reason-native","version":env!("CARGO_PKG_VERSION")}})},
  "ping"=>json!({}),
  "tools/list"=>tools(local),
  "tools/call"=>{
   let Some(params)=request["params"].as_object()else{return Some(rpc_error(id,-32602,"Invalid tool parameters"))};
   if !params.contains_key("name")||!params.contains_key("arguments")||params.keys().any(|k|!matches!(k.as_str(),"name"|"arguments"|"_meta"))||params.get("_meta").is_some_and(|v|!v.is_object()) {return Some(rpc_error(id,-32602,"Invalid tool parameters"))}
   let result=match (controller.as_mut(),params["name"].as_str()){(Some(c),Some(name))=>{if name=="dispatch"&&params["arguments"]["template"].as_str().is_none_or(|t|if local{!matches!(t,"sum-native-v1"|"control-native-v1")}else{!matches!(t,"sum-v1"|"control-v1")}){Err(anyhow::anyhow!("mode template mismatch"))}else{c.call(name,params["arguments"].clone(),now)}},_=>Err(anyhow::anyhow!("AO_SESSION_ID missing or invalid"))};
   match result {Ok(value)=>json!({"content":[{"type":"text","text":value.to_string()}]}),Err(_)=>json!({"isError":true,"content":[{"type":"text","text":"Action rejected; check trusted host identity, exact task state and arguments"}]})}
  },
  _=>return Some(rpc_error(id,-32601,"Method not found")),
 };Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}
pub fn serve(input:&mut impl BufRead,output:&mut impl Write,mut controller:Option<ControlService>,local:bool)->Result<()> {
 loop {let mut frame=Vec::new();let count=(&mut *input).take(65538).read_until(b'\n',&mut frame)?;if count==0{return Ok(())}ensure!(frame.len()<=65536&&frame.ends_with(b"\n"),"MCP frame exceeds 64KiB or lacks newline");
 let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
 let response=match serde_json::from_slice::<Value>(&frame){Ok(request)=>handle(request,&mut controller,local,now),Err(_)=>Some(rpc_error(Value::Null,-32700,"Parse error"))};
 if let Some(response)=response{serde_json::to_writer(&mut *output,&response)?;output.write_all(b"\n")?;output.flush()?;}}
}
