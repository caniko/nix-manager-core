//! Cancellation keeps worker process groups and retained graph lifetimes aligned.
use super::*;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct Worker(std::process::Child);

impl Drop for Worker {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            // The child created its own process group before exec. Killing that
            // group cannot target the caller or other backend workers.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
}

impl Native {
    /// Execute a local exact-output worker with bounded signal cleanup. The
    /// caller must keep retained graph roots alive until this function returns.
    pub fn realise_cancellable(&self, node: &Node, cancel: &AtomicBool) -> Result<()> {
        let mut args = self.build_args(false, node.restore_only);
        args.push(node.output.installable()?);
        let result = self.command_cancellable(&args, cancel)?;
        ensure!(
            result.status.success(),
            "Nix realization failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        ensure!(
            self.valid(&node.path)?,
            "Nix returned without the requested output {}",
            node.path
        );
        Ok(())
    }

    pub(super) fn command_cancellable(
        &self,
        args: &[String],
        cancel: &AtomicBool,
    ) -> Result<ProcessOutput> {
        ensure!(
            self.nix.is_absolute() && self.timeout.is_absolute(),
            "backend tools must be absolute paths"
        );
        ensure!(
            (1..=86_400).contains(&self.timeout_seconds),
            "backend timeout out of range"
        );
        ensure!(
            !cancel.load(Ordering::SeqCst),
            "Nix worker cancelled before execution"
        );
        // Files avoid pipe backpressure while the controller watches signals.
        let stdout = tempfile::tempfile()?;
        let stderr = tempfile::tempfile()?;
        let mut worker = Worker(
            Command::new(&self.timeout)
                .args(["--signal=TERM", "--kill-after=10s"])
                .arg(self.timeout_seconds.to_string())
                .arg(&self.nix)
                .args(args)
                .env("LC_ALL", "C")
                .env("NO_COLOR", "1")
                .stdout(stdout.try_clone()?)
                .stderr(stderr.try_clone()?)
                .process_group(0)
                .spawn()
                .context("start cancellable Nix worker")?,
        );
        let mut interrupted = None;
        let status = loop {
            if let Some(status) = worker.0.try_wait()? {
                if interrupted.is_some() {
                    // A shell may exit on SIGINT while background descendants
                    // ignore it. Drain the group before releasing caller roots.
                    unsafe {
                        libc::kill(-(worker.0.id() as i32), libc::SIGKILL);
                    }
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while group_is_live(worker.0.id())? {
                        ensure!(
                            Instant::now() < deadline,
                            "cancelled Nix process group did not drain"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                break status;
            }
            if cancel.load(Ordering::SeqCst) && interrupted.is_none() {
                unsafe {
                    libc::kill(-(worker.0.id() as i32), libc::SIGINT);
                }
                interrupted = Some(Instant::now());
            }
            if interrupted.is_some_and(|since| since.elapsed() >= Duration::from_secs(10)) {
                unsafe {
                    libc::kill(-(worker.0.id() as i32), libc::SIGKILL);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        use std::io::{Read, Seek};
        let read = |mut file: fs::File| -> Result<Vec<u8>> {
            file.rewind()?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        ensure!(
            interrupted.is_none(),
            "Nix worker cancelled after process-group cleanup"
        );
        Ok(ProcessOutput {
            status,
            stdout: read(stdout)?,
            stderr: read(stderr)?,
        })
    }
}

fn group_is_live(group: u32) -> Result<bool> {
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            continue;
        };
        let fields: Vec<_> = fields.split_whitespace().take(3).collect();
        if fields.len() == 3 && fields[0] != "Z" && fields[2].parse::<u32>().ok() == Some(group) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn cancellation_drains_the_owned_process_group_before_returning() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("nix");
        let pidfile = root.path().join("descendant");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ntrap 'exit 0' INT TERM\nsleep 30 &\necho $! > '{}'\nwait\n",
                pidfile.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let backend = Native {
            nix: script,
            timeout: PathBuf::from("/usr/bin/env"),
            timeout_seconds: 60,
            query_timeout_seconds: 1,
            system: "x86_64-linux".into(),
            gc_roots: root.path().into(),
            substitutes: false,
        };
        // Use a transparent timeout fixture, so the real process group contains
        // both the worker and a descendant instead of testing a mocked signal.
        let timeout = root.path().join("timeout");
        fs::write(&timeout, "#!/bin/sh\nshift 3\nexec \"$@\"\n").unwrap();
        fs::set_permissions(&timeout, fs::Permissions::from_mode(0o700)).unwrap();
        let backend = Native { timeout, ..backend };
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                cancel.store(true, Ordering::SeqCst);
            });
            assert!(backend
                .command_cancellable(&[], &cancel)
                .unwrap_err()
                .to_string()
                .contains("cancelled"));
        });
        assert!(started.elapsed() < Duration::from_secs(12));
        let pid = fs::read_to_string(pidfile).unwrap();
        if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
            assert!(
                stat.rsplit_once(") ").unwrap().1.starts_with('Z'),
                "descendant remained live after cancellation"
            );
        }
    }
}
