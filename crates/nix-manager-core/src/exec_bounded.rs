//! Deadline-bound execution with byte-preserving file logs.

use std::{
    fs::File,
    io,
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

/// Limits belong to this invocation; they never signal an existing lease holder.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub timeout: Duration,
    pub termination_grace: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(300),
            termination_grace: Duration::from_secs(5),
        }
    }
}

/// A timeout remains a failure even if a termination handler returns exit zero.
#[derive(Debug)]
pub struct Outcome {
    pub status: ExitStatus,
    pub timed_out: bool,
    pub cancelled: bool,
    pub elapsed: Duration,
}

impl Outcome {
    #[must_use]
    pub fn success(&self) -> bool {
        !self.timed_out && !self.cancelled && self.status.success()
    }
}

/// Pre-spawn errors prove this invocation did not launch. Errors after spawn
/// cannot establish whether external effects occurred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorPhase {
    Preparation,
    Spawn,
    Observation,
    Termination,
}

#[derive(Debug)]
pub struct RunError {
    pub phase: ErrorPhase,
    pub source: io::Error,
    pub elapsed: Duration,
}

impl RunError {
    #[must_use]
    pub fn was_spawned(&self) -> bool {
        matches!(
            self.phase,
            ErrorPhase::Observation | ErrorPhase::Termination
        )
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "bounded execution {:?}: {}",
            self.phase, self.source
        )
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Execute without a shell, streaming unmodified stdout/stderr to caller-owned
/// files. Arguments, environment values and log contents are never printed.
/// The caller controls stdin, working directory, credentials and log retention.
///
/// On Unix, a new process group confines deadline cancellation to this child and
/// its descendants. The leader is kept unreaped until cancellation finishes so
/// its process-group identity cannot be recycled during the grace period.
pub fn run(
    command: &mut Command,
    limits: Limits,
    stdout: &File,
    stderr: &File,
) -> io::Result<Outcome> {
    run_controlled(command, limits, stdout, stderr, &AtomicBool::new(false))
        .map_err(|error| error.source)
}

/// Execute once with caller-owned cancellation and phase-attributable errors.
/// Cancellation before spawn refuses execution. After spawn it uses the same
/// owned process-group TERM/grace/KILL boundary as a deadline. Partial logs are
/// retained, and callers must reconcile any possible external mutation.
pub fn run_controlled(
    command: &mut Command,
    limits: Limits,
    stdout: &File,
    stderr: &File,
    cancellation: &AtomicBool,
) -> Result<Outcome, RunError> {
    let started = Instant::now();
    let failed = |phase, source| RunError {
        phase,
        source,
        elapsed: started.elapsed(),
    };
    if limits.timeout.is_zero() {
        return Err(failed(
            ErrorPhase::Preparation,
            io::Error::new(io::ErrorKind::InvalidInput, "deadline must be positive"),
        ));
    }
    if cancellation.load(Ordering::Acquire) {
        return Err(failed(
            ErrorPhase::Preparation,
            io::Error::new(io::ErrorKind::Interrupted, "cancelled before spawn"),
        ));
    }
    command.stdout(Stdio::from(
        stdout
            .try_clone()
            .map_err(|error| failed(ErrorPhase::Preparation, error))?,
    ));
    command.stderr(Stdio::from(
        stderr
            .try_clone()
            .map_err(|error| failed(ErrorPhase::Preparation, error))?,
    ));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| failed(ErrorPhase::Spawn, error))?;
    loop {
        // Check invocation controls before observing/reaping an exit. Otherwise
        // an exit between polls can erase an already elapsed deadline or a
        // caller cancellation, and cancellation can lose the group leader.
        let cancelled = cancellation.load(Ordering::Acquire);
        let timed_out = started.elapsed() >= limits.timeout;
        if cancelled || timed_out {
            let status = terminate(&mut child, limits.termination_grace)
                .map_err(|error| failed(ErrorPhase::Termination, error))?;
            return Ok(Outcome {
                status,
                timed_out,
                cancelled,
                elapsed: started.elapsed(),
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Outcome {
                    status,
                    timed_out: false,
                    cancelled: false,
                    elapsed: started.elapsed(),
                });
            }
            Ok(None) => {}
            Err(error) => {
                // Preserve the original wait error after making a bounded effort
                // to stop and reap the process owned by this invocation.
                let _ = terminate(&mut child, limits.termination_grace);
                return Err(failed(ErrorPhase::Observation, error));
            }
        }
        thread::sleep(
            Duration::from_millis(10).min(limits.timeout.saturating_sub(started.elapsed())),
        );
    }
}

fn terminate(child: &mut Child, grace: Duration) -> io::Result<ExitStatus> {
    #[cfg(unix)]
    {
        let group = i32::try_from(child.id())
            .map_err(|_| io::Error::other("child process-group identifier exceeds pid_t"))?;
        signal_group(group, libc::SIGTERM)?;
        thread::sleep(grace);
        signal_group(group, libc::SIGKILL)?;
        #[cfg(target_os = "linux")]
        {
            // Keep the leader unreaped while checking its group: its PID/PGID
            // cannot be reused to identify unrelated work during cleanup.
            let deadline = Instant::now() + Duration::from_secs(10);
            while group_is_live(group)? {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned process group did not drain",
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = grace;
        child.kill()?;
    }
    child.wait()
}

#[cfg(target_os = "linux")]
fn group_is_live(group: i32) -> io::Result<bool> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if let Some((_, fields)) = stat.rsplit_once(") ") {
            let fields: Vec<_> = fields.split_whitespace().take(3).collect();
            if fields.len() == 3
                && !matches!(fields[0], "Z" | "X")
                && fields[2].parse::<i32>().ok() == Some(group)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn signal_group(group: i32, signal: i32) -> io::Result<()> {
    // SAFETY: spawn assigned the unreaped owned child its own process group.
    // A negative pid signals that group, never the caller or a lease holder.
    if unsafe { libc::kill(-group, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command, time::Duration};

    fn logs() -> (tempfile::TempDir, std::fs::File, std::fs::File) {
        let directory = tempfile::tempdir().unwrap();
        let stdout = std::fs::File::create(directory.path().join("stdout")).unwrap();
        let stderr = std::fs::File::create(directory.path().join("stderr")).unwrap();
        (directory, stdout, stderr)
    }

    #[test]
    fn preserves_raw_bytes_and_nonzero_exit() {
        let (directory, stdout, stderr) = logs();
        let mut command = Command::new("sh");
        command.args(["-c", "printf '\\377\\000x'; printf 'failure' >&2; exit 7"]);
        let result = run(&mut command, Limits::default(), &stdout, &stderr).unwrap();
        assert!(!result.success());
        assert!(!result.timed_out);
        assert_eq!(result.status.code(), Some(7));
        assert_eq!(
            fs::read(directory.path().join("stdout")).unwrap(),
            [255, 0, b'x']
        );
        assert_eq!(
            fs::read(directory.path().join("stderr")).unwrap(),
            b"failure"
        );
    }

    #[test]
    fn timeout_is_failure_even_when_term_handler_exits_successfully() {
        let (_directory, stdout, stderr) = logs();
        let mut command = Command::new("sh");
        command.args(["-c", "trap 'exit 0' TERM; while :; do sleep 0.01; done"]);
        let result = run(
            &mut command,
            Limits {
                timeout: Duration::from_millis(150),
                termination_grace: Duration::from_secs(1),
            },
            &stdout,
            &stderr,
        )
        .unwrap();
        assert!(result.timed_out);
        assert!(!result.success());
    }

    #[cfg(unix)]
    #[test]
    fn timeout_terminates_own_process_group_and_retains_partial_logs() {
        let (directory, stdout, stderr) = logs();
        let marker = directory.path().join("escaped");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "trap '' TERM; (sleep 0.8; touch \"$1\") & printf 'partial'; wait",
            "fixture",
        ]);
        command.arg(&marker);
        let result = run(
            &mut command,
            Limits {
                timeout: Duration::from_millis(150),
                termination_grace: Duration::from_millis(50),
            },
            &stdout,
            &stderr,
        )
        .unwrap();
        assert!(result.timed_out);
        std::thread::sleep(Duration::from_secs(1));
        assert!(!marker.exists());
        assert_eq!(
            fs::read(directory.path().join("stdout")).unwrap(),
            b"partial"
        );
    }

    #[test]
    fn zero_deadline_refuses_before_spawning() {
        let (directory, stdout, stderr) = logs();
        let marker = directory.path().join("spawned");
        let mut command = Command::new("touch");
        command.arg(&marker);
        assert!(
            run(
                &mut command,
                Limits {
                    timeout: Duration::ZERO,
                    termination_grace: Duration::ZERO,
                },
                &stdout,
                &stderr
            )
            .is_err()
        );
        assert!(!marker.exists());
    }

    #[test]
    fn cancelled_before_spawn_is_attributably_not_executed() {
        let (directory, stdout, stderr) = logs();
        let marker = directory.path().join("spawned");
        let mut command = Command::new("touch");
        command.arg(&marker);
        let error = run_controlled(
            &mut command,
            Limits::default(),
            &stdout,
            &stderr,
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert_eq!(error.phase, ErrorPhase::Preparation);
        assert!(!error.was_spawned());
        assert_eq!(error.source.kind(), io::ErrorKind::Interrupted);
        assert!(!marker.exists());
    }

    #[test]
    fn unavailable_program_is_attributably_not_executed() {
        let (directory, stdout, stderr) = logs();
        let mut command = Command::new(directory.path().join("absent-program"));
        let error = run_controlled(
            &mut command,
            Limits::default(),
            &stdout,
            &stderr,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.phase, ErrorPhase::Spawn);
        assert!(!error.was_spawned());
        assert_eq!(error.source.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn caller_cancellation_drains_descendants_and_preserves_partial_logs() {
        let (directory, stdout, stderr) = logs();
        let marker = directory.path().join("escaped");
        let cancelled = AtomicBool::new(false);
        let outcome = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(150));
                cancelled.store(true, Ordering::Release);
            });
            let mut command = Command::new("sh");
            command.args([
                "-c",
                "trap '' TERM; (sleep 0.8; touch \"$1\") & printf 'partial'; wait",
                "fixture",
            ]);
            command.arg(&marker);
            run_controlled(
                &mut command,
                Limits {
                    timeout: Duration::from_secs(5),
                    termination_grace: Duration::from_millis(50),
                },
                &stdout,
                &stderr,
                &cancelled,
            )
            .unwrap()
        });
        assert!(outcome.cancelled);
        assert!(!outcome.timed_out);
        assert!(!outcome.success());
        std::thread::sleep(Duration::from_secs(1));
        assert!(!marker.exists());
        assert_eq!(
            fs::read(directory.path().join("stdout")).unwrap(),
            b"partial"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cancellation_keeps_other_invocations_alive_and_drains_before_returning() {
        let (directory, stdout, stderr) = logs();
        let mut unrelated = Command::new("sleep").arg("30").spawn().unwrap();
        let child_pid = directory.path().join("descendant");
        let cancelled = AtomicBool::new(false);
        let outcome = std::thread::scope(|scope| {
            scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !child_pid.exists() {
                    assert!(Instant::now() < deadline, "fixture did not start");
                    std::thread::sleep(Duration::from_millis(10));
                }
                cancelled.store(true, Ordering::Release);
            });
            let mut command = Command::new("sh");
            command.args(["-c", "trap 'exit 0' TERM; sh -c 'trap \"\" TERM; exec sleep 30' & echo $! > \"$1\"; wait", "fixture"]);
            command.arg(&child_pid);
            run_controlled(
                &mut command,
                Limits {
                    timeout: Duration::from_secs(5),
                    termination_grace: Duration::from_millis(50),
                },
                &stdout,
                &stderr,
                &cancelled,
            )
            .unwrap()
        });
        assert!(outcome.cancelled && !outcome.success());
        let pid = fs::read_to_string(child_pid).unwrap();
        if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
            assert!(matches!(
                stat.rsplit_once(") ").unwrap().1.split_whitespace().next(),
                Some("Z" | "X")
            ));
        }
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
    }
}
