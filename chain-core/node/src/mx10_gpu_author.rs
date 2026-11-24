//! Small bounded authoring adapter around the unchanged MX10 CUDA batch API.
//! Uses the host UUID flock and shared crash fence, never the V2 miner/kernel.
use super::{GPU_LOCK,GPU_UUID};
use crate::gpu_pow_mx10::{Certificate,VERSION};
use std::{fs::{self,File,OpenOptions},io::Write,path::{Path,PathBuf},process::{Command,Stdio},thread,time::{Duration,Instant,SystemTime,UNIX_EPOCH}};
use std::os::{fd::AsRawFd,unix::fs::{MetadataExt,OpenOptionsExt}};
unsafe extern "C" { fn flock(fd:i32,op:i32)->i32; fn geteuid()->u32; }
const ROOT:&str="/var/lib/eagle-ai/gpu-pow";
fn hex(bytes:&[u8])->String {bytes.iter().map(|b|format!("{b:02x}")).collect()}
fn digest(s:&str)->Result<[u8;32],String>{
 if s.len()!=64 {return Err("MX10 GPU invalid digest length".into());}
 let mut a=[0u8;32];for i in 0..32 {a[i]=u8::from_str_radix(&s[i*2..i*2+2],16).map_err(|e|e.to_string())?;}Ok(a)
}
fn command(mut cmd:Command,deadline:Instant)->Result<String,String>{
 let mut child=cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e|e.to_string())?;
 loop {
  if let Some(_)=child.try_wait().map_err(|e|e.to_string())? {let out=child.wait_with_output().map_err(|e|e.to_string())?;
   if !out.status.success(){return Err(format!("MX10 GPU helper failed: {}",String::from_utf8_lossy(&out.stderr)));}
   return String::from_utf8(out.stdout).map_err(|e|e.to_string());
  }
  if Instant::now()>=deadline {let _=child.kill();let _=child.wait();return Err("MX10 GPU helper timeout; child killed/reaped".into());}
  thread::sleep(Duration::from_millis(10));
 }
}
fn private_dir(path:&Path)->Result<(),String>{let m=fs::symlink_metadata(path).map_err(|e|e.to_string())?;
 if !m.is_dir()||m.file_type().is_symlink()||m.uid()!=unsafe{geteuid()}||m.mode()&0o077!=0 {return Err(format!("unsafe MX10 GPU owner directory {}",path.display()));}Ok(())}

pub struct GpuSession {lock:File,bin:PathBuf,start:Instant,deadline:Instant,global:PathBuf,active:PathBuf,complete:PathBuf,samples:Vec<serde_json::Value>,out:PathBuf,finished:bool}
impl GpuSession {
 pub fn open(out:&Path)->Result<Self,String>{
  let root=PathBuf::from(ROOT);let global=root.join(GPU_UUID);private_dir(&root)?;private_dir(&global)?;
  for marker in ["eagw-pow-v2-global-active","eagw-pow-v2-global-unknown","eagw-pow-v2-global-transition"] {
   if fs::symlink_metadata(global.join(marker)).is_ok(){return Err(format!("MX10 GPU unavailable: unresolved shared fence {marker}"));}
  }
  // Build before taking GPU ownership; nvcc does not execute a GPU kernel.
  let bin=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/mx10-author/mx10_gpu");
  fs::create_dir_all(bin.parent().unwrap()).map_err(|e|e.to_string())?;
  let source=bin.parent().unwrap().join("gpu_pow_mx10_gpu.cu");fs::write(&source,super::CUDA_SOURCE).map_err(|e|e.to_string())?;
  let mut nvcc=Command::new("nvcc");nvcc.args(["-O3","-arch=sm_86","-o"]).arg(&bin).arg(&source);
  command(nvcc,Instant::now()+Duration::from_secs(25))?;
  let lock=OpenOptions::new().read(true).write(true).custom_flags(0x20000).open(GPU_LOCK).map_err(|e|e.to_string())?;
  let m=lock.metadata().map_err(|e|e.to_string())?;let p=fs::symlink_metadata(GPU_LOCK).map_err(|e|e.to_string())?;
  if !m.is_file()||p.file_type().is_symlink()||m.uid()!=unsafe{geteuid()}||m.mode()&0o077!=0||m.dev()!=p.dev()||m.ino()!=p.ino(){return Err("unsafe MX10 GPU flock file".into());}
  if unsafe{flock(lock.as_raw_fd(),2|4)}!=0 {return Err("MX10 GPU flock busy; no GPU was used".into());}
  let start=Instant::now();let deadline=start+Duration::from_secs(50);
  for marker in ["eagw-pow-v2-global-active","eagw-pow-v2-global-unknown","eagw-pow-v2-global-transition"] {
   if fs::symlink_metadata(global.join(marker)).is_ok(){return Err(format!("MX10 GPU shared fence appeared: {marker}"));}
  }
  let active=global.join("eagw-pow-v2-global-active");let id=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();let complete=global.join(format!("mx10-author-complete-{}-{id}.json",std::process::id()));
  let mut this=Self{lock,bin,start,deadline,global,active,complete,samples:Vec::new(),out:out.to_path_buf(),finished:false};
  this.sample()?;
  let mut apps=Command::new("nvidia-smi");apps.arg(format!("--id={GPU_UUID}")).args(["--query-compute-apps=gpu_uuid,pid","--format=csv,noheader,nounits"]);
  let apps=command(apps,this.limit(2))?;if !apps.trim().is_empty(){return Err("MX10 GPU has existing compute applications; no kernel launched".into());}
  let mut marker=OpenOptions::new().write(true).create_new(true).mode(0o600).open(&this.active).map_err(|e|e.to_string())?;
  write!(marker,"{{\"schema\":\"SVY-MX10-AUTHOR-GPU-OWNERSHIP-1\",\"gpu_uuid\":\"{GPU_UUID}\",\"pid\":{},\"crash_requires_reconciliation\":true}}",std::process::id()).map_err(|e|e.to_string())?;marker.sync_all().map_err(|e|e.to_string())?;
  Ok(this)
 }
 fn limit(&self,seconds:u64)->Instant {self.deadline.min(Instant::now()+Duration::from_secs(seconds))}
 fn sample(&mut self)->Result<(),String>{
  if Instant::now()>=self.deadline{return Err("MX10 GPU 50-second lease deadline".into());}
  let mut cmd=Command::new("nvidia-smi");cmd.arg(format!("--id={GPU_UUID}")).args(["--query-gpu=uuid,temperature.gpu,power.draw","--format=csv,noheader,nounits"]);
  let s=command(cmd,self.limit(2))?;let f:Vec<_>=s.trim().split(',').map(str::trim).collect();
  if f.len()!=3||f[0]!=GPU_UUID {return Err("MX10 GPU UUID preflight mismatch".into());}
  let temperature:f64=f[1].parse().map_err(|_|"bad GPU temperature")?;let power:f64=f[2].parse().map_err(|_|"bad GPU power")?;
  self.samples.push(serde_json::json!({"elapsed_seconds":self.start.elapsed().as_secs_f64(),"uuid":f[0],"temperatureC":temperature,"powerW":power}));
  fs::write(self.out.join("gpu-telemetry.json"),serde_json::to_vec_pretty(&self.samples).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
  if temperature>=78.0{return Err("MX10 GPU thermal stop at >=78C".into());}Ok(())
 }
 pub fn batch(&mut self,challenge:[u8;32],base:u64,count:u32)->Result<Vec<Certificate>,String>{
  if count==0||count>16||base.checked_add(u64::from(count)-1).is_none(){return Err("invalid bounded MX10 GPU batch".into());}
  self.sample()?;
  let mut cmd=Command::new(&self.bin);cmd.args(["batch","16",&base.to_string(),&count.to_string(),&hex(&challenge)]).env("CUDA_VISIBLE_DEVICES",GPU_UUID);
  // Child stdout is bounded to <=16 lines (<4KiB), so pipe capacity cannot
  // stall this wait. Retain flock while the child exists; kill+reap on timeout.
  let mut child=cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e|e.to_string())?;
  let end=self.limit(3);let mut monitor=Instant::now()+Duration::from_millis(250);
  loop {
   if child.try_wait().map_err(|e|e.to_string())?.is_some(){break;}
   if Instant::now()>=end {let _=child.kill();let _=child.wait();return Err("MX10 GPU batch deadline; child killed/reaped".into());}
   if Instant::now()>=monitor {
    if let Err(e)=self.sample(){let _=child.kill();let _=child.wait();return Err(e);}
    monitor=Instant::now()+Duration::from_millis(250);
   }
   thread::sleep(Duration::from_millis(10));
  }
  let output=child.wait_with_output().map_err(|e|e.to_string())?;if !output.status.success(){return Err(format!("MX10 GPU batch failed: {}",String::from_utf8_lossy(&output.stderr)));}
  self.sample()?;let text=String::from_utf8(output.stdout).map_err(|e|e.to_string())?;let mut result=Vec::new();
  for (i,line) in text.lines().enumerate(){let f:Vec<_>=line.split_whitespace().collect();if f.len()!=4||f[0]!="16"||f[1].parse::<u64>().ok()!=Some(base+i as u64){return Err("MX10 GPU batch identity/order mismatch".into());}
   result.push(Certificate{version:VERSION,challenge,nonce:base+i as u64,input_digest:digest(f[2])?,output_digest:digest(f[3])?});
  }
  if result.len()!=count as usize{return Err("MX10 GPU incomplete batch".into());}
  let mut log=OpenOptions::new().append(true).create(true).open(self.out.join("gpu-batches.jsonl")).map_err(|e|e.to_string())?;
  writeln!(log,"{}",serde_json::json!({"n":16,"base":base,"count":count,"challenge":hex(&challenge),"raw_stdout":text,"kernel":"unchanged gpu_pow_mx10_gpu.cu batch mode"})).map_err(|e|e.to_string())?;Ok(result)
 }
 pub fn finish(mut self)->Result<serde_json::Value,String>{
  self.sample()?;let elapsed=self.start.elapsed().as_secs_f64();
  let mut report=serde_json::json!({"uuid":GPU_UUID,"lock":GPU_LOCK,"lock_acquired":true,"occupancy_seconds_before_final_marker_sync":elapsed,"lease_limit_seconds":50,"hard_requirement_seconds":60,"temperature_stopC":78,"power_changed":false,"samples":self.samples,"completion_marker":self.complete.to_string_lossy()});
  fs::write(&self.active,serde_json::to_vec_pretty(&report).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
  File::open(&self.active).and_then(|f|f.sync_all()).map_err(|e|e.to_string())?;
  fs::rename(&self.active,&self.complete).map_err(|e|e.to_string())?;File::open(&self.global).and_then(|f|f.sync_all()).map_err(|e|e.to_string())?;
  if unsafe{flock(self.lock.as_raw_fd(),8)}!=0 {return Err("MX10 GPU unlock failed".into());}
  report["occupancy_seconds"]=self.start.elapsed().as_secs_f64().into();
  self.finished=true;
  fs::write(self.out.join("gpu-session.json"),serde_json::to_vec_pretty(&report).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
  // Ownership ends on drop immediately after this function returns.
  Ok(report)
 }
}
impl Drop for GpuSession {fn drop(&mut self){
 // On any error retain the shared durable active fence. This blocks further
 // GPU work until explicit reconciliation. OS flock is released on drop.
 let _=&self.lock;
 if !self.finished && fs::symlink_metadata(&self.active).is_ok() {let _=fs::write(self.out.join("GPU-STOPPED-REQUIRES-REVIEW.json"),format!("{{\"shared_active_fence_retained\":true,\"elapsed_seconds\":{}}}",self.start.elapsed().as_secs_f64()));}
}}
