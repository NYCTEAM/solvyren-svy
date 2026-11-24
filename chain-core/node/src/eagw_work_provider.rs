//! Experimental Linux/WSL one-shot EAGW work provider for local authoring.
//! Output is CPU-recomputed and does not attest physical GPU execution.

use crate::{gpu_work_authoring::WorkProvider, gpu_work_binding::recompute_cpu_work};
use futures::{channel::oneshot, future::BoxFuture};
use sp_core::hashing::sha2_256;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::{
    fs::{MetadataExt, OpenOptionsExt},
    process::CommandExt,
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub(crate) struct LocalGpuWorkProvider {
    pub python: PathBuf,
    pub script: PathBuf,
    pub attempt_parent: PathBuf,
    pub max_duration: Duration,
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl WorkProvider for LocalGpuWorkProvider {
    fn produce(
        &self,
        subject: [u8; 32],
        deadline: Instant,
    ) -> BoxFuture<'static, Result<[u8; 32], String>> {
        let this = self.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let (tx, rx) = oneshot::channel();
        // Spawn happens on first poll; dropping the future before then starts nothing.
        Box::pin(async move {
            let _guard = CancelOnDrop(cancelled);
            std::thread::Builder::new()
                .name("eagw-once".into())
                .spawn(move || {
                    let _ = tx.send(this.run(subject, deadline, flag));
                })
                .map_err(|e| format!("cannot spawn supervisor: {e}"))?;
            rx.await
                .map_err(|_| "work supervisor terminated unexpectedly".to_owned())?
        })
    }
}

impl LocalGpuWorkProvider {
    /// Authorities must opt into explicit, absolute local paths. This prevents
    /// silently running a PATH-resolved interpreter or an unpinned workspace.
    pub(crate) fn from_environment() -> Result<Self, String> {
        fn absolute(name: &str) -> Result<PathBuf, String> {
            let value =
                std::env::var_os(name).ok_or_else(|| format!("authority requires {name}"))?;
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(format!("{name} must be an absolute path"));
            }
            Ok(path)
        }

        let seconds = std::env::var("EAGW_MAX_SECONDS")
            .map_err(|_| "authority requires EAGW_MAX_SECONDS".to_owned())?
            .parse::<u64>()
            .map_err(|_| "EAGW_MAX_SECONDS must be an integer".to_owned())?;
        if !(1..=60).contains(&seconds) {
            return Err("EAGW_MAX_SECONDS must be 1..=60".into());
        }
        Ok(Self {
            python: absolute("EAGW_PYTHON")?,
            script: absolute("EAGW_SCRIPT")?,
            attempt_parent: absolute("EAGW_ATTEMPT_PARENT")?,
            max_duration: Duration::from_secs(seconds),
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(unix)]
fn read_exact_file(path: &Path, size: usize) -> Result<Vec<u8>, String> {
    // Linux O_NOFOLLOW; reject hardlinks and detect mutation across read.
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(0x20000)
        .open(path)
        .map_err(|e| e.to_string())?;
    let before = f.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() || before.nlink() != 1 || before.len() != size as u64 {
        return Err("invalid evidence file".into());
    }
    let mut data = Vec::with_capacity(size);
    f.read_to_end(&mut data).map_err(|e| e.to_string())?;
    let after = f.metadata().map_err(|e| e.to_string())?;
    if data.len() != size
        || (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        )
    {
        return Err("evidence changed during read".into());
    }
    Ok(data)
}

#[cfg(unix)]
fn gpu_lock(parent: &Path, deadline: Instant, cancelled: &AtomicBool) -> Result<File, String> {
    // All authorities must share this directory; the lock inode must persist.
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    let path = parent.join("eagw-gpu.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(0x20000)
        .open(&path)
        .map_err(|e| format!("open GPU lock: {e}"))?;
    let opened = lock.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file() || opened.nlink() != 1 {
        return Err("invalid GPU lock file".into());
    }
    loop {
        if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return Err("GPU lock wait cancelled or expired".into());
        }
        if unsafe { flock(lock.as_raw_fd(), 2 | 4) } == 0 {
            let current = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !current.is_file()
                || current.nlink() != 1
                || (opened.dev(), opened.ino()) != (current.dev(), current.ino())
            {
                return Err("GPU lock identity changed".into());
            }
            if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline {
                return Err("GPU lock acquired after cancellation or deadline".into());
            }
            if parent.join("eagw-pow-v2-active").exists()
                || parent.join("eagw-pow-v2-unknown").exists()
            {
                return Err("unresolved PoW GPU attempt blocks shared GPU work".into());
            }
            return Ok(lock);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(format!("GPU lock failed: {error}"));
        }
        std::thread::sleep(
            Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(unix)]
impl LocalGpuWorkProvider {
    fn run(
        &self,
        subject: [u8; 32],
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<[u8; 32], String> {
        if !self.python.is_absolute()
            || !self.script.is_absolute()
            || !self.attempt_parent.is_absolute()
            || self.max_duration.is_zero()
            || self.max_duration > Duration::from_secs(60)
        {
            return Err("invalid absolute paths or duration".into());
        }
        if cancelled.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let deadline = deadline.min(Instant::now() + self.max_duration);
        let _gpu_lock = gpu_lock(&self.attempt_parent, deadline, &cancelled)?;
        if deadline <= Instant::now() {
            return Err("deadline elapsed".into());
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let dir = self.attempt_parent.join(format!(
            "eagw-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).map_err(|e| format!("fresh attempt directory: {e}"))?;
        let output = dir.join("output");
        let mut intent = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join("intent.txt"))
            .map_err(|e| e.to_string())?;
        writeln!(
            intent,
            "subject={} status=started retry=forbidden",
            hex(&subject)
        )
        .map_err(|e| e.to_string())?;
        intent.sync_all().map_err(|e| e.to_string())?;
        File::open(&dir)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&self.python);
        cmd.arg(&self.script)
            .arg(hex(&subject))
            .arg(&output)
            .arg("--timeout")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return Err("cancelled before launch".into());
        }
        let mut child = cmd.spawn().map_err(|e| format!("launch failed: {e}"))?;
        loop {
            if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline {
                // The Python supervisor and its spawn child inherit this group.
                unsafe extern "C" {
                    fn kill(pid: i32, signal: i32) -> i32;
                }
                let killed = unsafe { kill(-(child.id() as i32), 9) };
                let waited = child.wait();
                return Err(format!(
                    "cancelled/timeout; group kill={killed}, wait={waited:?}; GPU cleanup unknown"
                ));
            }
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) if status.success() => break,
                Some(status) => return Err(format!("worker failed: {status}; state unknown")),
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return Err("deadline after work".into());
        }
        let input = read_exact_file(&output.join("input.bin"), 18 * 2048 * 4)?;
        let output_bytes = read_exact_file(&output.join("output.bin"), 128)?;
        // Presence of result is an audit artifact only; it is not consensus input.
        let result_meta =
            fs::symlink_metadata(output.join("result.json")).map_err(|e| e.to_string())?;
        if !result_meta.is_file() || result_meta.len() > 16384 {
            return Err("missing or oversized result evidence".into());
        }
        let reference = recompute_cpu_work(subject);
        let expected: Vec<u8> = reference
            .output
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        if sha2_256(&input) != reference.input_digest
            || output_bytes != expected
            || sha2_256(&output_bytes) != reference.output_digest
        {
            return Err("CPU work verification failed".into());
        }
        if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return Err("deadline after CPU verification".into());
        }
        Ok(reference.commitment)
    }
}

#[cfg(not(unix))]
impl LocalGpuWorkProvider {
    fn run(
        &self,
        _subject: [u8; 32],
        _deadline: Instant,
        _cancelled: Arc<AtomicBool>,
    ) -> Result<[u8; 32], String> {
        Err("Windows-to-WSL process tree supervision is unavailable".into())
    }
}
