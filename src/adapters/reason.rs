//! Native Reason JSON-line IPC. Explicit endpoint, no CLI/shell or daemon startup.
use anyhow::{Result,Context,ensure};
use serde_json::{Value,json};
use std::time::{Duration,Instant};
use crate::core::{protocol::Envelope,relay::RelayTransport};
const MAX:usize=1024*1024;
const MAX_RELAY_FRAME:usize=64*1024;
pub struct Reason {endpoint:String,timeout:Duration}

pub struct ReasonRelayTransport<'a> { reason: &'a Reason, from: &'a str }

impl<'a> ReasonRelayTransport<'a> {
 pub fn new(reason: &'a Reason, from: &'a str)->Result<Self>{
  ensure!(!from.is_empty()&&from.len()<=256&&from.is_ascii()&&!from.chars().any(char::is_whitespace),"invalid Reason from");
  Ok(Self{reason,from})
 }
}

impl RelayTransport for ReasonRelayTransport<'_> {
 fn send(&mut self,destination:&str,frame:&[u8])->Result<()> {
  ensure!(frame.len()<=MAX_RELAY_FRAME,"relay frame exceeds 64KiB");
  let body=std::str::from_utf8(frame).context("Reason frame must be strict UTF-8")?;
  let message:Envelope=serde_json::from_slice(frame).context("invalid Reason relay envelope")?;
  ensure!(crate::config::valid_id(&message.message_id),"invalid Reason relay message id");
  self.reason.send(self.from,destination,&message.message_id,body)?;
  Ok(())
 }
}
impl Reason {
 pub fn new(endpoint:String)->Result<Self>{ensure!(!endpoint.is_empty()&&endpoint.len()<=4096,"invalid Reason endpoint");#[cfg(windows)]ensure!(endpoint.starts_with(r"\\.\pipe\"),"explicit local named pipe required");#[cfg(unix)]ensure!(std::path::Path::new(&endpoint).is_absolute(),"absolute Unix socket required");Ok(Self{endpoint,timeout:Duration::from_secs(3)})}
 pub fn call(&self,op:&str,args:Value)->Result<Value>{
  ensure!(matches!(op,"send"|"inbox"|"delivered"),"unsupported Reason operation");let mut request=args.as_object().context("IPC args must be object")?.clone();ensure!(!request.contains_key("id")&&!request.contains_key("op"),"reserved IPC fields");let id=uuid::Uuid::new_v4().to_string();request.insert("id".into(),json!(id));request.insert("op".into(),json!(op));let mut bytes=serde_json::to_vec(&request)?;ensure!(bytes.len()<MAX,"Reason request bound");bytes.push(b'\n');
  let raw=exchange(&self.endpoint,&bytes,self.timeout)?;let reply:Value=serde_json::from_slice(&raw)?;ensure!(reply["id"]==id&&reply["ok"]==true,"Reason response identity/refusal");Ok(reply.get("result").cloned().context("Reason result missing")?)
 }
 pub fn send(&self,from:&str,to:&str,id:&str,body:&str)->Result<Value>{ensure!(body.len()<=65536,"native message bound");let receipt=self.call("send",json!({"idempotency_key":id,"from":from,"to":to,"body":body,"wake":false}))?;let recipients=receipt["recipients"].as_array().context("Reason recipients missing")?;ensure!(recipients.len()==1&&recipients[0]["address"]==to&&matches!(recipients[0]["state"].as_str(),Some("queued"|"accepted"|"delivered")),"Reason ambiguous recipient receipt");ensure!(receipt["ids"].as_array().is_some_and(|a|!a.is_empty()&&a.iter().all(|v|v.as_str().is_some_and(|s|!s.is_empty()))),"Reason receipt ids missing");Ok(receipt)}
 pub fn inbox(&self,key:&str)->Result<Vec<(String,String)>>{let value=self.call("inbox",json!({"key":key,"wait_secs":1,"peek":true}))?;let messages=value["messages"].as_array().context("Reason inbox missing")?;ensure!(messages.len()<=100,"Reason inbox batch bound");messages.iter().map(|m|{let id=m["id"].as_str().context("inbox id missing")?;let body=m["body"].as_str().context("inbox body missing")?;ensure!(!id.is_empty()&&id.len()<=512&&body.len()<=65536,"inbox bound");Ok((id.into(),body.into()))}).collect()}
 pub fn delivered(&self,key:&str,id:&str)->Result<()> {self.call("delivered",json!({"key":key,"ids":[id]}))?;Ok(())}
}
#[cfg(unix)]
fn exchange(endpoint:&str,request:&[u8],timeout:Duration)->Result<Vec<u8>>{
 use std::io::{Read,Write};use std::os::unix::net::UnixStream;
 let end=Instant::now()+timeout;let mut stream=UnixStream::connect(endpoint)?;stream.set_read_timeout(Some(timeout))?;stream.set_write_timeout(Some(timeout))?;stream.write_all(request)?;let mut frame=Vec::new();loop{let left=end.checked_duration_since(Instant::now()).context("Reason IPC deadline")?;stream.set_read_timeout(Some(left))?;let mut b=[0;4096];let n=stream.read(&mut b)?;ensure!(n>0,"Reason EOF before newline");if let Some(i)=b[..n].iter().position(|v|*v==b'\n'){ensure!(frame.len()+i<=MAX,"Reason response bound");frame.extend_from_slice(&b[..i]);return Ok(frame)}frame.extend_from_slice(&b[..n]);ensure!(frame.len()<=MAX,"Reason response bound");}
}
#[cfg(windows)]
fn exchange(endpoint:&str,request:&[u8],timeout:Duration)->Result<Vec<u8>>{
 use windows_sys::Win32::{Foundation::{HANDLE,CloseHandle,INVALID_HANDLE_VALUE,GENERIC_READ,GENERIC_WRITE,GetLastError,ERROR_PIPE_BUSY,ERROR_IO_PENDING,WAIT_OBJECT_0},Storage::FileSystem::{CreateFileW,OPEN_EXISTING,FILE_FLAG_OVERLAPPED,ReadFile,WriteFile},System::{Pipes::WaitNamedPipeW,IO::{OVERLAPPED,GetOverlappedResult,CancelIoEx},Threading::{CreateEventW,WaitForSingleObject}}};
 struct Handle(HANDLE);impl Drop for Handle{fn drop(&mut self){unsafe{CloseHandle(self.0);}}}
 let end=Instant::now()+timeout;let wide:Vec<u16>=endpoint.encode_utf16().chain(Some(0)).collect();
 let raw=loop{let raw=unsafe{CreateFileW(wide.as_ptr(),GENERIC_READ|GENERIC_WRITE,0,std::ptr::null(),OPEN_EXISTING,FILE_FLAG_OVERLAPPED,std::ptr::null_mut())};if raw!=INVALID_HANDLE_VALUE{break raw}let code=unsafe{GetLastError()};ensure!(code==ERROR_PIPE_BUSY,"Reason pipe open failed ({code})");let left=end.checked_duration_since(Instant::now()).context("Reason pipe connect deadline")?;ensure!(unsafe{WaitNamedPipeW(wide.as_ptr(),left.as_millis().min(u32::MAX as u128) as u32)}!=0,"Reason pipe wait failed");};let pipe=Handle(raw);
 fn transfer(pipe:HANDLE,buffer:*mut u8,len:usize,write:bool,end:Instant)->Result<usize>{
  let event=Handle(unsafe{CreateEventW(std::ptr::null(),1,0,std::ptr::null())});ensure!(!event.0.is_null(),"Reason IPC event unavailable");let mut overlapped=OVERLAPPED{hEvent:event.0,..Default::default()};let mut count=0;
  let ok=unsafe{if write{WriteFile(pipe,buffer,len as u32,&mut count,&mut overlapped)}else{ReadFile(pipe,buffer,len as u32,&mut count,&mut overlapped)}};
  if ok==0 {let code=unsafe{GetLastError()};ensure!(code==ERROR_IO_PENDING,"Reason IPC I/O failed ({code})");let wait=end.saturating_duration_since(Instant::now()).as_millis().min(u32::MAX as u128) as u32;
   if unsafe{WaitForSingleObject(event.0,wait)}!=WAIT_OBJECT_0 {unsafe{CancelIoEx(pipe,&overlapped);GetOverlappedResult(pipe,&overlapped,&mut count,1);}anyhow::bail!("Reason IPC deadline; operation cancelled");}
   ensure!(unsafe{GetOverlappedResult(pipe,&overlapped,&mut count,0)}!=0,"Reason IPC completion failed");
  }Ok(count as usize)
 }
 let mut sent=0;while sent<request.len(){let n=transfer(pipe.0,request[sent..].as_ptr() as *mut u8,request.len()-sent,true,end)?;ensure!(n>0,"Reason write EOF");sent+=n;}
 let mut frame=Vec::new();loop{let mut buffer=[0u8;4096];let n=transfer(pipe.0,buffer.as_mut_ptr(),buffer.len(),false,end)?;ensure!(n>0,"Reason EOF before newline");if let Some(i)=buffer[..n].iter().position(|b|*b==b'\n'){ensure!(frame.len()+i<=MAX,"Reason response bound");frame.extend_from_slice(&buffer[..i]);return Ok(frame)}frame.extend_from_slice(&buffer[..n]);ensure!(frame.len()<=MAX,"Reason response bound");}
}
