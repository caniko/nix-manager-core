//! Deadline-bound execution with byte-preserving file logs.

use std::{
    fs::File,
    io,
    process::{Child, Command, ExitStatus, Stdio},
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
    pub elapsed: Duration,
}

impl Outcome {
    #[must_use]
    pub fn success(&self) -> bool {
        !self.timed_out && self.status.success()
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
    if limits.timeout.is_zero() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "deadline must be positive"));
    }
    command.stdout(Stdio::from(stdout.try_clone()?));
    command.stderr(Stdio::from(stderr.try_clone()?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let started = Instant::now();
    let mut child = command.spawn()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Outcome { status, timed_out: false, elapsed: started.elapsed() });
            }
            Ok(None) => {}
            Err(error) => {
                // Preserve the original wait error after making a bounded effort
                // to stop and reap the process owned by this invocation.
                let _ = terminate(&mut child, limits.termination_grace);
                return Err(error);
            }
        }
        if started.elapsed() >= limits.timeout {
            let status = terminate(&mut child, limits.termination_grace)?;
            return Ok(Outcome { status, timed_out: true, elapsed: started.elapsed() });
        }
        thread::sleep(Duration::from_millis(10).min(limits.timeout.saturating_sub(started.elapsed())));
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
    }
    #[cfg(not(unix))]
    {
        let _ = grace;
        child.kill()?;
    }
    child.wait()
}

#[cfg(unix)]
fn signal_group(group: i32, signal: i32) -> io::Result<()> {
    // SAFETY: spawn assigned the unreaped owned child its own process group.
    // A negative pid signals that group, never the caller or a lease holder.
    if unsafe { libc::kill(-group, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(error) }
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
        assert_eq!(fs::read(directory.path().join("stdout")).unwrap(), [255, 0, b'x']);
        assert_eq!(fs::read(directory.path().join("stderr")).unwrap(), b"failure");
    }

    #[test]
    fn timeout_is_failure_even_when_term_handler_exits_successfully() {
        let (_directory, stdout, stderr) = logs();
        let mut command = Command::new("sh");
        command.args(["-c", "trap 'exit 0' TERM; while :; do sleep 0.01; done"]);
        let result = run(&mut command, Limits {
            timeout: Duration::from_millis(150),
            termination_grace: Duration::from_secs(1),
        }, &stdout, &stderr).unwrap();
        assert!(result.timed_out);
        assert!(!result.success());
    }

    #[cfg(unix)]
    #[test]
    fn timeout_terminates_own_process_group_and_retains_partial_logs() {
        let (directory, stdout, stderr) = logs();
        let marker = directory.path().join("escaped");
        let mut command = Command::new("sh");
        command.args(["-c", "trap '' TERM; (sleep 0.8; touch \"$1\") & printf 'partial'; wait", "fixture"]);
        command.arg(&marker);
        let result = run(&mut command, Limits {
            timeout: Duration::from_millis(150),
            termination_grace: Duration::from_millis(50),
        }, &stdout, &stderr).unwrap();
        assert!(result.timed_out);
        std::thread::sleep(Duration::from_secs(1));
        assert!(!marker.exists());
        assert_eq!(fs::read(directory.path().join("stdout")).unwrap(), b"partial");
    }

    #[test]
    fn zero_deadline_refuses_before_spawning() {
        let (directory, stdout, stderr) = logs();
        let marker = directory.path().join("spawned");
        let mut command = Command::new("touch");
        command.arg(&marker);
        assert!(run(&mut command, Limits {
            timeout: Duration::ZERO,
            termination_grace: Duration::ZERO,
        }, &stdout, &stderr).is_err());
        assert!(!marker.exists());
    }
}
