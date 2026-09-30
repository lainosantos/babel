//! Separate executors for deadlines, processing and the control plane.
//!
//! Only device transport and short PCM operations belong on `audio_handle`.
//! Providers, commands, history and file writers use `processing_handle`; their
//! blocking pool cannot consume the device workers. The runtimes live until
//! process exit so dropping a controller in async code never drops a runtime.
//!
//! Linux/Windows reserve up to two *logical* CPUs among Babel's executor workers when
//! affinity is available. This is not hardware exclusivity: other applications,
//! SMT siblings and memory bandwidth remain outside Babel's control. macOS uses
//! separate bounded executors and native CoreAudio scheduling, not CPU pinning.
use std::{ffi::OsStr, sync::OnceLock};

use anyhow::{Context, Result, anyhow};
use tokio::{
    process::Command,
    runtime::{Builder, Handle, Runtime},
};

const AUDIO_WORKERS: usize = 2;
// Four transport workers plus native endpoint leases and short device queries.
const AUDIO_BLOCKING_WORKERS: usize = 12;

#[derive(Debug)]
struct CpuPlan {
    audio: Vec<usize>,
    background: Vec<usize>,
    background_capacity: usize,
}

impl CpuPlan {
    fn new(mut allowed: Vec<usize>, available: usize) -> Self {
        allowed.sort_unstable();
        allowed.dedup();
        // A quota of one CPU must not be treated as multiple CPUs just because
        // its affinity mask spans the host. Always leave at least one worker.
        let can_partition = available > 1 && allowed.len() > 1;
        let reserved = if available >= 4 && allowed.len() >= 4 {
            2
        } else {
            1
        };
        let audio = if can_partition {
            allowed.drain(..reserved).collect()
        } else {
            Vec::new()
        };
        let background_capacity = available
            .saturating_sub(if available >= 4 { 2 } else { 1 })
            .max(1)
            .min(if can_partition {
                allowed.len()
            } else {
                usize::MAX
            });
        Self {
            audio,
            background: if can_partition { allowed } else { Vec::new() },
            background_capacity,
        }
    }

    fn background_workers(&self) -> usize {
        self.background_capacity.min(2)
    }
}

fn cpu_plan() -> &'static CpuPlan {
    static PLAN: OnceLock<CpuPlan> = OnceLock::new();
    PLAN.get_or_init(|| {
        let available = std::thread::available_parallelism().map_or(1, usize::from);
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        let allowed = affinity::get_thread_affinity().unwrap_or_else(|error| {
            tracing::warn!("CPU affinity unavailable; executors remain isolated: {error}");
            Vec::new()
        });
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let allowed = Vec::new();
        CpuPlan::new(allowed, available)
    })
}

fn apply_affinity(audio: bool) {
    let plan = cpu_plan();
    let cpus = if audio { &plan.audio } else { &plan.background };
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if !cpus.is_empty()
        && let Err(error) = affinity::set_thread_affinity(cpus)
    {
        // Affinity is an optimization, never a reason to take devices offline.
        static REPORTED: OnceLock<()> = OnceLock::new();
        REPORTED.get_or_init(|| {
            tracing::warn!(
                "Could not reserve routing CPU; separate executors remain active: {error}"
            )
        });
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let _ = cpus;
}

/// Apply the audio CPU mask once at the start of a backend-owned callback
/// thread. Windows callback threads need this because thread affinity is not
/// automatically inherited from the executor worker that opens the stream.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) fn configure_audio_thread() {
    apply_affinity(true);
}

fn runtime(audio: bool, name: &'static str) -> Result<Runtime> {
    let plan = cpu_plan(); // Capture the original mask before worker hooks run.
    Builder::new_multi_thread()
        .worker_threads(if audio {
            AUDIO_WORKERS
        } else {
            plan.background_workers()
        })
        .max_blocking_threads(if audio {
            AUDIO_BLOCKING_WORKERS
        } else {
            plan.background_capacity.clamp(2, 4)
        })
        .thread_name(name)
        .on_thread_start(move || apply_affinity(audio))
        .enable_all()
        .build()
        .with_context(|| format!("Could not start {name} executor"))
}

/// A bounded runtime for HTTP, settings and lifecycle supervision.
pub fn control_runtime() -> Result<Runtime> {
    runtime(false, "babel-control")
}

/// Dedicated audio I/O runtime, including its own reserved blocking pool.
pub fn audio_handle() -> Result<Handle> {
    static AUDIO: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();
    shared_handle(&AUDIO, true, "babel-audio")
}

/// Bounded, separate runtime for providers, transcripts, commands and storage.
pub fn processing_handle() -> Result<Handle> {
    static PROCESSING: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();
    shared_handle(&PROCESSING, false, "babel-processing")
}

fn shared_handle(
    cell: &'static OnceLock<std::result::Result<Runtime, String>>,
    audio: bool,
    name: &'static str,
) -> Result<Handle> {
    match cell.get_or_init(|| runtime(audio, name).map_err(|error| format!("{error:#}"))) {
        Ok(runtime) => Ok(runtime.handle().clone()),
        Err(error) => Err(anyhow!(error.clone())),
    }
}

/// The configured inference limit is a ceiling; never consume audio's budget.
/// This is not a global CPU quota for independently running external services.
pub fn inference_threads(requested: u32) -> u32 {
    requested.max(1).min(cpu_plan().background_capacity as u32)
}

/// Launch an owned non-audio helper below the normal scheduling priority.
/// Unix uses the OS's `nice` utility before exec, so subsequently created model
/// threads inherit it. If a minimal Unix system omits nice, warn and retain the
/// separate bounded executors; never fail routing over scheduling policy.
/// On Linux, helpers inherit their processing worker's background CPU mask.
/// Windows children inherit process affinity, not thread affinity; priority is
/// lowered at creation, but their CPU mask is not claimed to be exclusive.
pub fn background_command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(unix)]
    {
        if let Some(nice) = ["/usr/bin/nice", "/bin/nice"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file())
        {
            let mut command = Command::new(nice);
            command.args(["-n", "10", "--"]).arg(program);
            configure_background_process(&mut command);
            return command;
        }
        static REPORTED: OnceLock<()> = OnceLock::new();
        REPORTED.get_or_init(|| tracing::warn!("OS nice utility unavailable; helper priority unchanged, executor isolation remains active"));
    }
    let mut command = Command::new(program);
    configure_background_process(&mut command);
    command
}

/// Bound common compute libraries, including supplied commands/fixtures.
/// A provider's explicit thread flags remain the authoritative GGML controls.
pub fn configure_background_process(command: &mut Command) {
    let threads = inference_threads(2).to_string();
    for name in [
        "OMP_NUM_THREADS",
        "OPENBLAS_NUM_THREADS",
        "MKL_NUM_THREADS",
        "NUMEXPR_NUM_THREADS",
        "VECLIB_MAXIMUM_THREADS",
        "RAYON_NUM_THREADS",
    ] {
        command.env(name, &threads);
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000 | 0x0000_4000); // NO_WINDOW | BELOW_NORMAL
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn cpu_partition_respects_restricted_masks_and_single_cpu_quotas() {
        let plan = CpuPlan::new(vec![9, 5, 6], 3);
        assert_eq!(plan.audio, [5]);
        assert_eq!(plan.background, [6, 9]);
        assert_eq!(plan.background_capacity, 2);
        let plan = CpuPlan::new(vec![2, 4, 6, 8], 4);
        assert_eq!(plan.audio, [2, 4]);
        assert_eq!(plan.background, [6, 8]);
        assert_eq!(plan.background_capacity, 2);
        for allowed in [vec![], vec![5], vec![5, 9]] {
            let plan = CpuPlan::new(allowed, 1);
            assert!(plan.audio.is_empty());
            assert!(plan.background.is_empty());
            assert_eq!(plan.background_capacity, 1);
        }
    }

    #[test]
    fn processing_saturation_cannot_starve_audio_executor_or_device_pool() {
        // Use the production builder but private runtimes: deliberately
        // saturating the global processing executor would interfere with
        // concurrent MCP/provider tests rather than testing this boundary.
        let processing_runtime = runtime(false, "babel-processing").unwrap();
        let audio_runtime = runtime(true, "babel-audio").unwrap();
        let processing = processing_runtime.handle();
        let audio = audio_runtime.handle();
        let (ready_tx, ready_rx) = mpsc::channel();
        let mut releases = Vec::new();
        // Deliberately block every processing executor worker. Dropping the
        // release senders also unblocks them if any assertion below panics.
        for _ in 0..cpu_plan().background_workers() {
            let ready = ready_tx.clone();
            let (release, wait) = mpsc::channel::<()>();
            releases.push(release);
            processing.spawn(async move {
                ready.send(()).unwrap();
                let _ = wait.recv();
            });
        }
        for _ in 0..releases.len() {
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let (done_tx, done_rx) = mpsc::channel();
        audio.spawn(async move {
            let name = std::thread::current().name().unwrap().to_owned();
            let blocking_name =
                tokio::task::spawn_blocking(|| std::thread::current().name().unwrap().to_owned())
                    .await
                    .unwrap();
            done_tx.send((name, blocking_name)).unwrap();
        });
        let (worker, blocking) = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(worker, "babel-audio");
        assert_eq!(blocking, "babel-audio");
        drop(releases);
    }

    #[test]
    fn inference_limit_leaves_the_reserved_capacity_untouched() {
        assert_eq!(inference_threads(0), 1);
        assert!(inference_threads(u32::MAX) as usize <= cpu_plan().background_capacity);
    }

    #[cfg(unix)]
    #[test]
    fn helper_priority_wrapper_preserves_program_and_argument_boundaries() {
        let mut command = background_command("/a folder/helper");
        command.args(["literal argument", "$(never execute)"]);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        if command
            .as_std()
            .get_program()
            .to_string_lossy()
            .ends_with("/nice")
        {
            assert_eq!(
                args,
                [
                    "-n",
                    "10",
                    "--",
                    "/a folder/helper",
                    "literal argument",
                    "$(never execute)"
                ]
            );
        } else {
            assert_eq!(command.as_std().get_program(), "/a folder/helper");
            assert_eq!(args, ["literal argument", "$(never execute)"]);
        }
    }

    #[test]
    #[ignore = "private execution-policy child fixture"]
    fn execution_policy_child() {
        if std::env::var_os("BABEL_TEST_EXECUTION_POLICY").is_none() {
            return;
        }
        for name in [
            "OMP_NUM_THREADS",
            "OPENBLAS_NUM_THREADS",
            "MKL_NUM_THREADS",
            "NUMEXPR_NUM_THREADS",
            "VECLIB_MAXIMUM_THREADS",
            "RAYON_NUM_THREADS",
        ] {
            let limit = std::env::var(name).unwrap().parse::<u32>().unwrap();
            assert!((1..=2).contains(&limit));
        }
        #[cfg(target_os = "linux")]
        if std::env::var_os("BABEL_TEST_EXPECT_NICE").is_some() {
            let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
            let fields: Vec<_> = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .collect();
            assert!(fields[16].parse::<i32>().unwrap() >= 10);
        }
        println!("BABEL_EXECUTION_POLICY_OK");
    }

    #[tokio::test]
    async fn owned_helper_has_compute_limits_before_it_starts() {
        let mut command = background_command(std::env::current_exe().unwrap());
        if command
            .as_std()
            .get_program()
            .to_string_lossy()
            .ends_with("/nice")
        {
            command.env("BABEL_TEST_EXPECT_NICE", "1");
        }
        let output = command
            .args(["--ignored", "execution_policy_child", "--nocapture"])
            .env("BABEL_TEST_EXECUTION_POLICY", "1")
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "execution policy child failed");
        assert!(String::from_utf8_lossy(&output.stdout).contains("BABEL_EXECUTION_POLICY_OK"));
    }
}
