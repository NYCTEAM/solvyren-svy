//! Bounded V3 GPU nonce search and durable submission fencing for an isolated
//! experimental PoW chain. A ticket proves a bounded SHA-256 claim, not GPU
//! origin, useful AI work, or production-ready mining economics.

use crate::{
    gpu_pow_v3::{self, Seal, SEAL_BYTES},
    gpu_pow_v3_consensus::target_for_difficulty,
};
use sp_core::{hashing::sha2_256, H256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::{
    fd::AsRawFd,
    unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
};

#[cfg(all(test, unix))]
use std::os::unix::fs::PermissionsExt;

const ACTIVE_FILE: &str = "eagw-pow-v3-active";
const UNKNOWN_FILE: &str = "eagw-pow-v3-unknown";
// All PoW protocol versions intentionally share these host-wide markers and
// GPU lock. V2 must see V3's ACTIVE state before it can use the same device.
const GLOBAL_ACTIVE_FILE: &str = "eagw-pow-v2-global-active";
const GLOBAL_UNKNOWN_FILE: &str = "eagw-pow-v2-global-unknown";
const GLOBAL_TRANSITION_FILE: &str = "eagw-pow-v2-global-transition";
const GLOBAL_RESOLVED_FILE: &str = "eagw-pow-v3-global-resolved";
#[cfg(unix)]
const HOST_OWNER_ROOT: &str = "/var/lib/eagle-ai/gpu-pow";
const AUTHORING_SESSION_FILE: &str = "eagw-pow-v3-authoring-session";
const AUTHORING_COMPLETE_FILE: &str = "eagw-pow-v3-authoring-complete";
const AUTHORING_STOPPED_FILE: &str = "eagw-pow-v3-authoring-stopped";

/// Local, explicitly configured one-shot V3 GPU runner.
#[derive(Clone)]
pub(crate) struct LocalGpuPowRunner {
    python: PathBuf,
    script: PathBuf,
    attempt_parent: PathBuf,
    owner_root: PathBuf,
    gpu_lock: PathBuf,
    global_state: PathBuf,
    nvidia_smi: PathBuf,
    expected_gpu_uuid: String,
    max_duration: Duration,
    max_attempts: u64,
    batch_size: u32,
    // The supervisor is sequential, but clones must share the same cursor so
    // a proven miss cannot restart the same nonce range on one candidate.
    next_nonce: Arc<Mutex<Option<([u8; 32], u128)>>>,
}

/// A validated seal plus the retained attempt directory used for submission
/// intent and outcome recording.
pub(crate) struct MinedSeal {
    pub seal: Vec<u8>,
    attempt: PathBuf,
}

impl LocalGpuPowRunner {
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
        let seconds = std::env::var("EAGW_POW_MAX_SECONDS")
            .map_err(|_| "authority requires EAGW_POW_MAX_SECONDS".to_owned())?
            .parse::<u64>()
            .map_err(|_| "EAGW_POW_MAX_SECONDS must be an integer".to_owned())?;
        if !(1..=60).contains(&seconds) {
            return Err("EAGW_POW_MAX_SECONDS must be 1..=60".into());
        }
        let max_attempts = std::env::var("EAGW_POW_MAX_ATTEMPTS")
            .map_err(|_| "authority requires EAGW_POW_MAX_ATTEMPTS".to_owned())?
            .parse::<u64>()
            .map_err(|_| "EAGW_POW_MAX_ATTEMPTS must be an integer".to_owned())?;
        if !(1..=(1u64 << 40)).contains(&max_attempts) {
            return Err("EAGW_POW_MAX_ATTEMPTS must be 1..=1099511627776".into());
        }
        let batch_size = std::env::var("EAGW_POW_BATCH_SIZE")
            .map_err(|_| "authority requires EAGW_POW_BATCH_SIZE".to_owned())?
            .parse::<u32>()
            .map_err(|_| "EAGW_POW_BATCH_SIZE must be an integer".to_owned())?;
        if !(1..=1_048_576).contains(&batch_size) {
            return Err("EAGW_POW_BATCH_SIZE must be 1..=1048576".into());
        }
        let expected_gpu_uuid = std::env::var("EAGW_POW_GPU_UUID")
            .map_err(|_| "authority requires EAGW_POW_GPU_UUID".to_owned())?;
        if !valid_gpu_uuid(&expected_gpu_uuid) {
            return Err("EAGW_POW_GPU_UUID must be a canonical GPU-xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx value".into());
        }
        #[cfg(unix)]
        let owner_root = PathBuf::from(HOST_OWNER_ROOT);
        #[cfg(not(unix))]
        let owner_root = PathBuf::from(r"C:\ProgramData\EagleAI\gpu-pow");
        let script = absolute("EAGW_POW_SCRIPT")?;
        if script.file_name().and_then(|name| name.to_str()) != Some("eagw_pow_v3_once.py") {
            return Err("V3 requires eagw_pow_v3_once.py".into());
        }
        let runner = Self {
            python: absolute("EAGW_POW_PYTHON")?,
            script,
            attempt_parent: absolute("EAGW_POW_ATTEMPT_PARENT")?,
            gpu_lock: owner_root.join(format!("{expected_gpu_uuid}.lock")),
            global_state: owner_root.join(&expected_gpu_uuid),
            owner_root,
            nvidia_smi: absolute("EAGW_POW_NVIDIA_SMI")?,
            expected_gpu_uuid,
            max_duration: Duration::from_secs(seconds),
            max_attempts,
            batch_size,
            next_nonce: Arc::new(Mutex::new(None)),
        };
        runner.validate_parent()?;
        runner.validate_owner_domain()?;
        runner.validate_global_state()?;
        Ok(runner)
    }

    fn validate_global_state(&self) -> Result<(), String> {
        let metadata = fs::symlink_metadata(&self.global_state)
            .map_err(|e| format!("PoW global state directory unavailable: {e}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("PoW global state must be a real directory".into());
        }
        if marker_exists_fail_closed(&self.global_state.join(GLOBAL_ACTIVE_FILE))?
            || marker_exists_fail_closed(&self.global_state.join(GLOBAL_UNKNOWN_FILE))?
            || marker_exists_fail_closed(&self.global_state.join(GLOBAL_TRANSITION_FILE))?
        {
            return Err("unresolved global PoW GPU state requires read-only reconciliation".into());
        }
        Ok(())
    }

    #[cfg(unix)]
    fn validate_owner_domain(&self) -> Result<(), String> {
        validate_private_owned_dir(&self.owner_root, "PoW host owner root")?;
        validate_private_owned_dir(&self.global_state, "PoW GPU state directory")?;
        let expected_lock = format!("{}.lock", self.expected_gpu_uuid);
        if self.global_state.parent() != Some(self.owner_root.as_path())
            || self.gpu_lock.parent() != Some(self.owner_root.as_path())
            || self
                .global_state
                .file_name()
                .and_then(|value| value.to_str())
                != Some(self.expected_gpu_uuid.as_str())
            || self.gpu_lock.file_name().and_then(|value| value.to_str())
                != Some(expected_lock.as_str())
        {
            return Err("PoW GPU owner paths are not canonically derived from the GPU UUID".into());
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn validate_owner_domain(&self) -> Result<(), String> {
        Err("gpu-pow-v3 requires native Linux host-owner supervision".into())
    }

    #[cfg(unix)]
    fn validate_gpu_preflight(&self, session_deadline: Instant) -> Result<(), String> {
        let deadline = (Instant::now() + Duration::from_secs(5)).min(session_deadline);
        let gpu_output = run_command_bounded(
            &self.nvidia_smi,
            &["--query-gpu=uuid", "--format=csv,noheader"],
            deadline,
        )?;
        let app_output = run_command_bounded(
            &self.nvidia_smi,
            &[
                "--query-compute-apps=gpu_uuid,pid,process_name,used_gpu_memory",
                "--format=csv,noheader,nounits",
            ],
            deadline,
        )?;
        validate_smi_outputs(&self.expected_gpu_uuid, &gpu_output, &app_output)
    }

    /// Reserve this work root for exactly one bounded authoring session. The
    /// create-new fence deliberately survives crashes and restarts: operators
    /// must reconcile the prior session and choose a fresh isolated work root
    /// instead of silently receiving a new GPU budget.
    pub(crate) fn reserve_authoring_session(
        &self,
        import_limit: u64,
        attempt_limit: u64,
        max_duration: Duration,
    ) -> Result<(), String> {
        self.validate_parent()?;
        self.validate_owner_domain()?;
        self.validate_global_state()?;
        write_new_synced(
            &self.attempt_parent.join(AUTHORING_SESSION_FILE),
            format!(
                "schema=EAGW-POW-V3-AUTHORING-SESSION\nimport_limit={import_limit}\nattempt_limit={attempt_limit}\nmax_seconds={}\nrestart_budget=forbidden\n",
                max_duration.as_secs()
            )
            .as_bytes(),
        )?;
        sync_dir(&self.attempt_parent)
    }

    pub(crate) fn record_authoring_attempt(&self, attempt: u64) -> Result<(), String> {
        write_new_synced(
            &self
                .attempt_parent
                .join(format!("eagw-pow-v3-authoring-attempt-{attempt:03}")),
            format!("attempt={attempt}\n").as_bytes(),
        )?;
        sync_dir(&self.attempt_parent)
    }

    pub(crate) fn record_authoring_complete(
        &self,
        imported: u64,
        attempts: u64,
    ) -> Result<(), String> {
        write_new_synced(
            &self.attempt_parent.join(AUTHORING_COMPLETE_FILE),
            format!(
                "schema=EAGW-POW-V3-AUTHORING-COMPLETE\nimported={imported}\nattempts={attempts}\n"
            )
            .as_bytes(),
        )?;
        sync_dir(&self.attempt_parent)
    }

    pub(crate) fn record_authoring_stopped(
        &self,
        imported: u64,
        attempts: u64,
        reason: &str,
    ) -> Result<(), String> {
        write_new_synced(
            &self.attempt_parent.join(AUTHORING_STOPPED_FILE),
            format!(
                "schema=EAGW-POW-V3-AUTHORING-STOPPED\nimported={imported}\nattempts={attempts}\nreason={}\n",
                single_line(reason)
            )
            .as_bytes(),
        )?;
        sync_dir(&self.attempt_parent)
    }

    fn validate_parent(&self) -> Result<(), String> {
        let metadata = fs::symlink_metadata(&self.attempt_parent)
            .map_err(|e| format!("PoW attempt parent unavailable: {e}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("PoW attempt parent must be a real directory".into());
        }
        if marker_exists_fail_closed(&self.attempt_parent.join(ACTIVE_FILE))?
            || marker_exists_fail_closed(&self.attempt_parent.join(UNKNOWN_FILE))?
        {
            return Err("unresolved PoW GPU attempt requires read-only reconciliation".into());
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn mine(
        &self,
        genesis: H256,
        parent_hash: H256,
        pre_hash: H256,
        difficulty: u128,
        session_deadline: Instant,
    ) -> Result<Option<MinedSeal>, String> {
        self.validate_parent()?;
        self.validate_owner_domain()?;
        self.validate_global_state()?;
        let deadline = (Instant::now() + self.max_duration).min(session_deadline);
        let _lock = gpu_lock(&self.gpu_lock, deadline)?;
        self.validate_gpu_preflight(deadline)?;
        self.validate_parent()?;
        self.validate_global_state()?;

        // Leave time for the Python supervisor to save its result and exit
        // before the outer process-tree deadline. A short remainder is a
        // known unstarted search, not an uncertain GPU attempt.
        let worker_seconds = deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
            .saturating_sub(2);
        if worker_seconds == 0 {
            return Ok(None);
        }

        let target = target_for_difficulty(difficulty).map_err(str::to_owned)?;

        let challenge = gpu_pow_v3::challenge_from_pre_hash(genesis, pre_hash);
        let start_nonce = {
            let cursor = self
                .next_nonce
                .lock()
                .map_err(|_| "V3 nonce cursor poisoned".to_owned())?;
            match *cursor {
                Some((previous, next)) if previous == challenge => next,
                _ => 0,
            }
        };
        if start_nonce > u128::from(u64::MAX) {
            return Err("V3 nonce space exhausted for this candidate".into());
        }
        let max_attempts =
            u128::from(self.max_attempts).min(u128::from(u64::MAX) + 1 - start_nonce) as u64;
        let start_nonce = start_nonce as u64;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let attempt = self
            .attempt_parent
            .join(format!("eagw-pow-v3-{}-{stamp}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&attempt)
            .map_err(|e| format!("fresh PoW attempt directory: {e}"))?;

        let active = self.attempt_parent.join(ACTIVE_FILE);
        write_new_synced(
            &active,
            format!(
                "schema=EAGW-POW-V3-ACTIVE\ngenesis={}\nparent_hash={}\npre_hash={}\nchallenge={}\ndifficulty={}\ntarget={}\nstart_nonce={}\nmax_attempts={}\nattempt={}\nretry=forbidden\n",
                hex(genesis.as_bytes()),
                hex(parent_hash.as_bytes()),
                hex(pre_hash.as_bytes()),
                hex(&challenge),
                difficulty,
                hex(&target),
                start_nonce,
                max_attempts,
                attempt.display(),
            )
            .as_bytes(),
        )?;
        sync_dir(&self.attempt_parent)?;
        let global_active = self.global_state.join(GLOBAL_ACTIVE_FILE);
        write_new_synced(
            &global_active,
            format!(
                "schema=EAGW-POW-V3-GLOBAL-ACTIVE\ngpu_uuid={}\nattempt={}\nretry=forbidden\n",
                self.expected_gpu_uuid,
                attempt.display(),
            )
            .as_bytes(),
        )
        .map_err(|error| self.unknown(&attempt, format!("global active fence: {error}")))?;
        sync_dir(&self.global_state)
            .map_err(|error| self.unknown(&attempt, format!("global active sync: {error}")))?;
        write_new_synced(
            &attempt.join("intent.txt"),
            format!(
                "schema=EAGW-POW-V3-INTENT\ngenesis={}\nparent_hash={}\npre_hash={}\nchallenge={}\ndifficulty={}\ntarget={}\nstart_nonce={}\nmax_attempts={}\nbatch_size={}\nretry=forbidden\n",
                hex(genesis.as_bytes()),
                hex(parent_hash.as_bytes()),
                hex(pre_hash.as_bytes()),
                hex(&challenge),
                difficulty,
                hex(&target),
                start_nonce,
                max_attempts,
                self.batch_size,
            )
            .as_bytes(),
        )?;
        sync_dir(&attempt)?;

        let output = attempt.join("output");
        let stdout = new_output(&attempt.join("stdout.log"))?;
        let stderr = new_output(&attempt.join("stderr.log"))?;
        let timeout_seconds = worker_seconds.to_string();
        let mut command = Command::new(&self.python);
        command
            .arg(&self.script)
            .arg(hex(&challenge))
            .arg(hex(&target))
            .arg(&output)
            .arg("--start-nonce")
            .arg(start_nonce.to_string())
            .arg("--max-attempts")
            .arg(max_attempts.to_string())
            .arg("--batch-size")
            .arg(self.batch_size.to_string())
            .arg("--timeout")
            .arg(timeout_seconds)
            .arg("--gpu-uuid")
            .arg(&self.expected_gpu_uuid)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|e| self.unknown(&attempt, format!("launch failed: {e}")))?;
        let child_start_ticks = match process_start_ticks(child.id()) {
            Ok(value) => value,
            Err(error) => {
                let reason = self.unknown(
                    &attempt,
                    format!("cannot bind worker process identity: {error}"),
                );
                let kill = child.kill();
                return Err(format!(
                    "{reason}; leader kill={kill:?}; process-group cleanup skipped because identity was not bound; UNKNOWN fences retained"
                ));
            }
        };
        loop {
            if Instant::now() >= deadline {
                let reason = self.unknown(
                    &attempt,
                    "deadline elapsed; worker cleanup and GPU state require review".into(),
                );
                let cleanup = terminate_owned_process_group(
                    &mut child,
                    child_start_ticks,
                    Instant::now() + Duration::from_secs(2),
                );
                let cleanup_record = format!("reason=deadline\n{cleanup}\n");
                let recorded =
                    write_new_synced(&attempt.join("cleanup.txt"), cleanup_record.as_bytes())
                        .and_then(|_| sync_dir(&attempt));
                return Err(format!(
                    "{reason}; cleanup={}; cleanup_record={recorded:?}; UNKNOWN fences retained",
                    single_line(&cleanup)
                ));
            }
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(status)) => {
                    return Err(self.unknown(
                        &attempt,
                        format!("worker failed: {status}; execution state unknown"),
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    let reason = self.unknown(
                        &attempt,
                        format!("cannot observe worker: {error}; execution state unknown"),
                    );
                    let cleanup = terminate_owned_process_group(
                        &mut child,
                        child_start_ticks,
                        Instant::now() + Duration::from_secs(2),
                    );
                    let cleanup_record = format!("reason=observe-error\n{cleanup}\n");
                    let recorded =
                        write_new_synced(&attempt.join("cleanup.txt"), cleanup_record.as_bytes())
                            .and_then(|_| sync_dir(&attempt));
                    return Err(format!(
                        "{reason}; cleanup={}; cleanup_record={recorded:?}; UNKNOWN fences retained",
                        single_line(&cleanup)
                    ));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(self.unknown(&attempt, "deadline after worker exit".into()));
        }

        let result = read_small_file(&output.join("result.json"), 16 * 1024)?;
        let result: serde_json::Value = serde_json::from_slice(&result)
            .map_err(|e| self.unknown(&attempt, format!("invalid result JSON: {e}")))?;
        if result.get("schema").and_then(|v| v.as_str()) != Some("EAGW-POW-V3-GPU-SEARCH")
            || result.get("challenge").and_then(|v| v.as_str()) != Some(&hex(&challenge))
            || result.get("target").and_then(|v| v.as_str()) != Some(&hex(&target))
            || result.get("gpuUuid").and_then(|v| v.as_str())
                != Some(self.expected_gpu_uuid.as_str())
            || result.get("startNonce").and_then(|v| v.as_u64()) != Some(start_nonce)
            || result.get("maxAttempts").and_then(|v| v.as_u64()) != Some(max_attempts)
            || result.get("batchSize").and_then(|v| v.as_u64()) != Some(u64::from(self.batch_size))
        {
            return Err(self.unknown(&attempt, "result binding mismatch".into()));
        }
        let attempts = result
            .get("attempts")
            .and_then(|v| v.as_u64())
            .filter(|value| *value <= max_attempts)
            .ok_or_else(|| self.unknown(&attempt, "result attempt count invalid".into()))?;
        match result.get("status").and_then(|v| v.as_str()) {
            Some("target-not-found") | Some("deadline-exhausted") => {
                let status = result.get("status").and_then(|v| v.as_str()).unwrap();
                let next_nonce = u128::from(start_nonce) + u128::from(attempts);
                write_new_synced(
                    &attempt.join("observed-miss.txt"),
                    format!("{status}\nstart_nonce={start_nonce}\nattempts={attempts}\nnext_nonce={next_nonce}\n").as_bytes(),
                )?;
                sync_dir(&attempt)?;
                if attempts > 0 {
                    *self
                        .next_nonce
                        .lock()
                        .map_err(|_| self.unknown(&attempt, "V3 nonce cursor poisoned".into()))? =
                        Some((challenge, next_nonce));
                }
                // clear_active_fences takes the same host GPU flock. The search
                // lock must be released after the definitive miss is durable.
                drop(_lock);
                self.clear_active_fences(&active)?;
                return Ok(None);
            }
            Some("found") => {}
            _ => return Err(self.unknown(&attempt, "unknown result status".into())),
        }

        let encoded = read_exact_file(&output.join("seal.bin"), SEAL_BYTES)
            .map_err(|error| self.unknown(&attempt, format!("seal read: {error}")))?;
        let seal = Seal::decode_fixed(&encoded)
            .map_err(|error| self.unknown(&attempt, format!("seal decode: {error:?}")))?;
        let nonce = result.get("nonce").and_then(|value| value.as_u64());
        if attempts == 0
            || nonce != Some(seal.nonce)
            || seal.nonce < start_nonce
            || seal.nonce - start_nonce >= attempts
            || seal.challenge != challenge
            || result.get("sealHex").and_then(|value| value.as_str()) != Some(&hex(&encoded))
            || result.get("sealSha256").and_then(|value| value.as_str())
                != Some(&hex(&sha2_256(&encoded)))
        {
            return Err(self.unknown(&attempt, "seal/result binding mismatch".into()));
        }
        let ticket = gpu_pow_v3::verify_pre_hash(genesis, pre_hash, &encoded, target)
            .map_err(|error| self.unknown(&attempt, format!("ticket verification: {error:?}")))?;
        if result.get("ticket").and_then(|value| value.as_str()) != Some(&hex(&ticket)) {
            return Err(self.unknown(&attempt, "ticket/result binding mismatch".into()));
        }
        write_new_synced(
            &attempt.join("validated.txt"),
            format!(
                "seal_sha256={}\nticket={}\nstart_nonce={}\nattempts={}\nnext_nonce={}\n",
                hex(&sha2_256(&encoded)),
                hex(&ticket),
                start_nonce,
                attempts,
                u128::from(start_nonce) + u128::from(attempts),
            )
            .as_bytes(),
        )?;
        sync_dir(&attempt)?;
        *self
            .next_nonce
            .lock()
            .map_err(|_| self.unknown(&attempt, "V3 nonce cursor poisoned".into()))? =
            Some((challenge, u128::from(start_nonce) + u128::from(attempts)));
        Ok(Some(MinedSeal {
            seal: encoded,
            attempt,
        }))
    }

    #[cfg(not(unix))]
    pub(crate) fn mine(
        &self,
        _genesis: H256,
        _parent_hash: H256,
        _pre_hash: H256,
        _difficulty: u128,
        _session_deadline: Instant,
    ) -> Result<Option<MinedSeal>, String> {
        Err("gpu-pow-v3 requires native Linux process-tree supervision".into())
    }

    pub(crate) fn record_submit_intent(&self, mined: &MinedSeal) -> Result<(), String> {
        write_new_synced(
            &mined.attempt.join("submit-intent.txt"),
            format!(
                "seal_sha256={}\nretry=forbidden\n",
                hex(&sha2_256(&mined.seal))
            )
            .as_bytes(),
        )?;
        sync_dir(&mined.attempt)
    }

    pub(crate) fn record_submit_observed(&self, mined: &MinedSeal) -> Result<(), String> {
        write_new_synced(
            &mined.attempt.join("submit-observed.txt"),
            b"import_returned=true\n",
        )?;
        sync_dir(&mined.attempt)?;
        self.clear_active_fences(&self.attempt_parent.join(ACTIVE_FILE))
    }

    pub(crate) fn record_submit_stale(&self, mined: &MinedSeal) -> Result<(), String> {
        write_new_synced(
            &mined.attempt.join("submit-stale.txt"),
            b"import_attempted=false\nreason=build generation changed\nretry_same_seal=forbidden\n",
        )?;
        sync_dir(&mined.attempt)?;
        self.clear_active_fences(&self.attempt_parent.join(ACTIVE_FILE))
    }

    pub(crate) fn record_submit_unknown(&self, mined: &MinedSeal, reason: &str) -> String {
        self.unknown(
            &mined.attempt,
            format!("submission outcome unknown: {reason}"),
        )
    }

    fn unknown(&self, attempt: &Path, reason: String) -> String {
        let detail = format!(
            "status=unknown\nreason={}\nretry=forbidden\n",
            single_line(&reason)
        );
        let mut persistence_errors = Vec::new();
        if let Err(error) = write_new_synced(&attempt.join("unknown.txt"), detail.as_bytes()) {
            persistence_errors.push(error);
        }
        if let Err(error) = write_new_synced(
            &self.attempt_parent.join(UNKNOWN_FILE),
            format!("attempt={}\n{}", attempt.display(), detail).as_bytes(),
        ) {
            persistence_errors.push(error);
        }
        if let Err(error) = write_new_synced(
            &self.global_state.join(GLOBAL_UNKNOWN_FILE),
            format!(
                "schema=EAGW-POW-V3-GLOBAL-UNKNOWN\ngpu_uuid={}\nattempt={}\n{}",
                self.expected_gpu_uuid,
                attempt.display(),
                detail,
            )
            .as_bytes(),
        ) {
            persistence_errors.push(error);
        }
        for directory in [
            attempt,
            self.attempt_parent.as_path(),
            self.global_state.as_path(),
        ] {
            if let Err(error) = sync_dir(directory) {
                persistence_errors.push(error);
            }
        }
        if persistence_errors.is_empty() {
            reason
        } else {
            format!(
                "{reason}; UNKNOWN persistence incomplete, retain all ACTIVE fences and inspect manually: {}",
                single_line(&persistence_errors.join("; "))
            )
        }
    }

    #[cfg(unix)]
    fn clear_active_fences(&self, local_active: &Path) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let _lock = gpu_lock(&self.gpu_lock, deadline)?;
        self.validate_owner_domain()?;
        let transition = self.global_state.join(GLOBAL_TRANSITION_FILE);
        write_new_synced(
            &transition,
            format!(
                "schema=EAGW-POW-V3-GLOBAL-TRANSITION\ngpu_uuid={}\nlocal_active={}\noutcome=definitive\n",
                self.expected_gpu_uuid,
                local_active.display()
            )
            .as_bytes(),
        )?;
        sync_dir(&self.global_state)?;
        clear_active(local_active, &self.attempt_parent)?;
        clear_active(
            &self.global_state.join(GLOBAL_ACTIVE_FILE),
            &self.global_state,
        )?;
        fs::rename(&transition, self.global_state.join(GLOBAL_RESOLVED_FILE))
            .map_err(|error| format!("resolve global PoW transition: {error}"))?;
        sync_dir(&self.global_state)
    }

    #[cfg(not(unix))]
    fn clear_active_fences(&self, _local_active: &Path) -> Result<(), String> {
        Err("gpu-pow-v3 requires native Linux host-owner supervision".into())
    }
}

fn single_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn marker_exists_fail_closed(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "cannot inspect PoW safety marker {}: {error}",
            path.display()
        )),
    }
}

#[cfg(unix)]
fn validate_private_owned_dir(path: &Path, label: &str) -> Result<(), String> {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("{label} unavailable: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!("{label} must be a real directory"));
    }
    let owner = unsafe { geteuid() };
    if metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "{label} must be owned by effective uid {owner} with no group/other permissions"
        ));
    }
    Ok(())
}

fn valid_gpu_uuid(value: &str) -> bool {
    value.len() == 40
        && value.starts_with("GPU-")
        && value.chars().enumerate().all(|(index, value)| match index {
            3 | 12 | 17 | 22 | 27 => value == '-',
            0..=2 => value.is_ascii_uppercase(),
            _ => value.is_ascii_hexdigit() && !value.is_ascii_uppercase(),
        })
}

fn validate_smi_outputs(expected: &str, gpu_output: &str, app_output: &str) -> Result<(), String> {
    let matches = gpu_output
        .lines()
        .map(str::trim)
        .filter(|value| *value == expected)
        .count();
    if matches != 1 {
        return Err("configured GPU UUID was not observed exactly once".into());
    }
    let conflicts: Vec<_> = app_output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| line.split(',').next().map(str::trim) == Some(expected))
        .collect();
    if !conflicts.is_empty() {
        return Err(format!(
            "configured GPU has existing compute applications: {}",
            single_line(&conflicts.join("; "))
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn process_start_ticks(pid: u32) -> Result<u64, String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|error| format!("read /proc identity: {error}"))?;
    let command_end = stat
        .rfind(')')
        .ok_or_else(|| "invalid /proc stat command field".to_owned())?;
    stat[command_end + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| "missing /proc start-time field".to_owned())?
        .parse::<u64>()
        .map_err(|error| format!("invalid /proc start-time field: {error}"))
}

#[cfg(unix)]
fn terminate_owned_process_group(
    child: &mut std::process::Child,
    expected_start_ticks: u64,
    deadline: Instant,
) -> String {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let pid = child.id();
    let identity = process_start_ticks(pid);
    if identity != Ok(expected_start_ticks) {
        return format!(
            "identity={identity:?}; signal=skipped; leader_wait=skipped; group_absence=unknown"
        );
    }
    let signal_result = unsafe { kill(-(pid as i32), 9) };
    let signal_error = if signal_result == 0 {
        None
    } else {
        Some(std::io::Error::last_os_error().to_string())
    };
    let mut leader_wait = "deadline".to_owned();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                leader_wait = format!("exited:{status}");
                break;
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => break,
            Err(error) => {
                leader_wait = format!("error:{error}");
                break;
            }
        }
    }
    let mut group_absence = "unknown".to_owned();
    while Instant::now() < deadline {
        if unsafe { kill(-(pid as i32), 0) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(3) {
                group_absence = "observed-esrch".into();
                break;
            }
            group_absence = format!("probe-error:{error}");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    format!(
        "identity=matched; signal_result={signal_result}; signal_error={signal_error:?}; leader_wait={leader_wait}; group_absence={group_absence}; gpu_cleanup=not-proven"
    )
}

#[cfg(unix)]
fn run_command_bounded(path: &Path, args: &[&str], deadline: Instant) -> Result<String, String> {
    unsafe extern "C" {
        fn fcntl(fd: i32, command: i32, ...) -> i32;
    }
    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 4;
    const O_NONBLOCK: i32 = 0x800;
    const OUTPUT_LIMIT: usize = 64 * 1024;

    fn drain<R: Read>(stream: &mut Option<R>, output: &mut Vec<u8>) -> Result<bool, String> {
        let Some(value) = stream.as_mut() else {
            return Ok(true);
        };
        let mut buffer = [0_u8; 8192];
        loop {
            match value.read(&mut buffer) {
                Ok(0) => {
                    *stream = None;
                    return Ok(true);
                }
                Ok(count) => {
                    output.extend_from_slice(&buffer[..count]);
                    if output.len() > OUTPUT_LIMIT {
                        return Err("GPU preflight output exceeded limit".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(format!("cannot read GPU preflight output: {error}")),
            }
        }
    }

    fn cleanup(child: &mut std::process::Child, identity: Option<u64>, reason: &str) -> String {
        let detail = match identity {
            Some(start_ticks) => terminate_owned_process_group(
                child,
                start_ticks,
                Instant::now() + Duration::from_secs(2),
            ),
            None => format!(
                "identity=unbound; leader_kill={:?}; process-group cleanup skipped",
                child.kill()
            ),
        };
        format!("{reason}; cleanup={detail}")
    }

    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("GPU preflight launch failed: {error}"))?;
    let identity = process_start_ticks(child.id()).ok();
    for fd in [
        child.stdout.as_ref().map(AsRawFd::as_raw_fd),
        child.stderr.as_ref().map(AsRawFd::as_raw_fd),
    ]
    .into_iter()
    .flatten()
    {
        let flags = unsafe { fcntl(fd, F_GETFL) };
        if flags < 0 || unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } < 0 {
            let error = std::io::Error::last_os_error();
            return Err(cleanup(
                &mut child,
                identity,
                &format!("cannot bound GPU preflight output: {error}"),
            ));
        }
    }
    let mut stdout_stream = child.stdout.take();
    let mut stderr_stream = child.stderr.take();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut status = None;
    loop {
        if Instant::now() >= deadline {
            return Err(cleanup(
                &mut child,
                identity,
                "GPU preflight deadline elapsed",
            ));
        }
        let stdout_eof = match drain(&mut stdout_stream, &mut stdout) {
            Ok(value) => value,
            Err(error) => return Err(cleanup(&mut child, identity, &error)),
        };
        let stderr_eof = match drain(&mut stderr_stream, &mut stderr) {
            Ok(value) => value,
            Err(error) => return Err(cleanup(&mut child, identity, &error)),
        };
        if status.is_none() {
            match child.try_wait() {
                Ok(value) => status = value,
                Err(error) => {
                    return Err(cleanup(
                        &mut child,
                        identity,
                        &format!("cannot observe GPU preflight: {error}"),
                    ));
                }
            }
        }
        if let Some(status) = status.as_ref() {
            if stdout_eof && stderr_eof {
                if !status.success() {
                    return Err(format!(
                        "GPU preflight failed: {}",
                        single_line(&String::from_utf8_lossy(&stderr))
                    ));
                }
                return String::from_utf8(stdout)
                    .map_err(|_| "GPU preflight output was not UTF-8".into());
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(0x20000);
    let mut file = options
        .open(path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

fn new_output(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(0x20000);
    options
        .open(path)
        .map_err(|e| format!("create {}: {e}", path.display()))
}

fn sync_dir(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("sync directory {}: {e}", path.display()))
}

fn clear_active(active: &Path, parent: &Path) -> Result<(), String> {
    fs::remove_file(active).map_err(|e| format!("remove active fence: {e}"))?;
    sync_dir(parent)
}

fn read_small_file(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max as u64 {
        return Err("invalid small evidence file".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

#[cfg(unix)]
fn read_exact_file(path: &Path, size: usize) -> Result<Vec<u8>, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(0x20000)
        .open(path)
        .map_err(|e| e.to_string())?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() || before.nlink() != 1 || before.len() != size as u64 {
        return Err("invalid evidence file".into());
    }
    let mut bytes = Vec::with_capacity(size);
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    if bytes.len() != size
        || (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
        )
    {
        return Err("evidence changed during read".into());
    }
    Ok(bytes)
}

#[cfg(unix)]
fn gpu_lock(path: &Path, deadline: Instant) -> Result<File, String> {
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(0x20000)
        .open(path)
        .map_err(|e| format!("open GPU lock: {e}"))?;
    let opened = lock.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file() || opened.nlink() != 1 {
        return Err("invalid GPU lock file".into());
    }
    loop {
        if Instant::now() >= deadline {
            return Err("GPU lock deadline elapsed".into());
        }
        if unsafe { flock(lock.as_raw_fd(), 2 | 4) } == 0 {
            let current = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
            if !current.is_file()
                || current.nlink() != 1
                || (opened.dev(), opened.ino()) != (current.dev(), current.ino())
            {
                return Err("GPU lock identity changed".into());
            }
            return Ok(lock);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(format!("GPU lock failed: {error}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
