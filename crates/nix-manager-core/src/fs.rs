//! Filesystem primitives shared by manager projects: durable atomic writes
//! and persistent-anchor `flock(2)` locks.

use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Atomically replace `path` with `bytes`, preserving an existing file's mode.
///
/// A new file is created with `new_file_mode`. The temporary file lives next
/// to the destination; it is fsynced before the rename and the parent
/// directory is fsynced after it, so a crash leaves either the old or the new
/// contents. Missing parent directories are created.
pub fn atomic_write(path: &Path, bytes: &[u8], new_file_mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mode = fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o7777)
        .unwrap_or(new_file_mode);
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary file in {}", parent.display()))?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .with_context(|| format!("write {}", path.display()))?;
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(mode))
        .with_context(|| format!("set mode {mode:o} for {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace {}", path.display()))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("fsync {}", parent.display()))?;
    Ok(())
}

/// Atomically write `bytes` to `path` with mode 0600.
///
/// Protocol: write to a tempfile adjacent to the destination, fsync, chmod,
/// verify mode, rename over destination, re-chmod, re-verify, fsync the parent.
/// This matches the canix `cache::recover::attic_config::atomic_write_0600`
/// protocol that every credential-file writer in the ecosystem must follow.
///
/// See AGENTS.md rule #2 in the canix repo for rationale.
pub fn atomic_write_0600(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp")
    ));
    let _ = fs::remove_file(&tmp);

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("fsync {}", tmp.display()))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
    verify_mode_0600(&tmp)?;
    drop(file);

    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} over {}", tmp.display(), path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))?;
    verify_mode_0600(path)?;

    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Verify that a file has exactly mode 0600.
pub fn verify_mode_0600(path: &Path) -> Result<()> {
    let mode = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions()
        .mode()
        & 0o777;
    if mode != 0o600 {
        return Err(anyhow!(
            "{} mode is {:o}, expected 600",
            path.display(),
            mode
        ));
    }
    Ok(())
}

/// Exclusive `flock(2)` over a persistent regular-file anchor.
///
/// The kernel releases the lock when the last descriptor sharing it closes,
/// including when the process dies by signal, so stale metadata can never
/// keep it held. Dropping the guard unlocks before closing, so a helper that
/// briefly inherited the descriptor between `fork` and `exec` cannot extend
/// it; after [`Self::inherit_across_exec`] dropping only closes, leaving the
/// lock with the exec'd program. The anchor is never unlinked: replacing the
/// pathname while another process holds the old inode would admit two
/// concurrent holders.
#[derive(Debug)]
pub struct ExclusiveFileLock {
    file: File,
    inherited: AtomicBool,
}

impl ExclusiveFileLock {
    /// Lock an existing anchor without waiting; `None` when another
    /// descriptor holds it. The anchor is opened read-only, so trusted
    /// root-owned anchors work for unprivileged callers.
    pub fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let file = open_anchor(path, false)?;
        match lock_exclusive_nonblocking(&file) {
            Ok(()) => Ok(Some(Self::held(file))),
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error).with_context(|| format!("failed to lock {}", path.display())),
        }
    }

    /// Lock an existing anchor or fail immediately, naming the holder.
    pub fn acquire(path: &Path, operation: &str) -> Result<Self> {
        Self::acquire_file(path, operation, open_anchor(path, false)?)
    }

    /// Lock an existing anchor, polling for at most `wait` before failing
    /// with the same holder diagnostic as [`Self::acquire`].
    pub fn acquire_wait(path: &Path, operation: &str, wait: Duration) -> Result<Self> {
        let started = Instant::now();
        loop {
            if let Some(lock) = Self::try_acquire(path)? {
                return Ok(lock);
            }
            let remaining = wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Self::acquire(path, operation);
            }
            std::thread::sleep(remaining.min(Duration::from_millis(250)));
        }
    }

    /// Create a mode-0600 anchor when missing, lock it or fail immediately,
    /// then clear any metadata a previous holder left in it.
    pub fn acquire_or_create(path: &Path, operation: &str) -> Result<Self> {
        let lock = Self::acquire_file(path, operation, open_anchor(path, true)?)?;
        lock.file
            .set_len(0)
            .with_context(|| format!("failed to clear stale lock metadata {}", path.display()))?;
        Ok(lock)
    }

    /// Use the trusted anchor when it exists; otherwise create and use
    /// `fallback` (for hosts that predate the trusted anchor).
    /// An anchor that exists but cannot be inspected is an error, never a
    /// reason to fall back.
    pub fn acquire_with_fallback(path: &Path, fallback: &Path, operation: &str) -> Result<Self> {
        if path
            .try_exists()
            .with_context(|| format!("failed to inspect lock anchor {}", path.display()))?
        {
            Self::acquire(path, operation)
        } else {
            Self::acquire_or_create(fallback, operation)
        }
    }

    fn held(file: File) -> Self {
        Self {
            file,
            inherited: AtomicBool::new(false),
        }
    }

    /// Keep the lock across `exec`: the replacement process then owns the
    /// kernel lock for its whole lifetime.
    pub fn inherit_across_exec(&self) -> Result<()> {
        self.inherited.store(true, Ordering::Relaxed);
        let fd = self.file.as_raw_fd();
        // SAFETY: fcntl only reads and updates descriptor flags of `fd`,
        // which `self.file` keeps open for the duration of both calls.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error()).context("read lock descriptor flags");
        }
        // SAFETY: as above.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error()).context("preserve lock across exec");
        }
        Ok(())
    }

    fn acquire_file(path: &Path, operation: &str, file: File) -> Result<Self> {
        match lock_exclusive_nonblocking(&file) {
            Ok(()) => return Ok(Self::held(file)),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to lock {}", path.display()));
            }
        }
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to inspect lock anchor {}", path.display()))?;
        let holder = lock_holder_pid(&metadata, false)
            .ok()
            .flatten()
            .map(format_holder)
            .unwrap_or_else(|| "(no matching FLOCK entry)".to_owned());
        bail!(
            "another {operation} is in progress\n  lock path: {}\n  held by:   {holder}",
            path.display()
        );
    }
}

impl AsFd for ExclusiveFileLock {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        if !self.inherited.load(Ordering::Relaxed) {
            // SAFETY: flock only observes the descriptor `self.file` still owns.
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

fn open_anchor(path: &Path, create: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    if create {
        options.create(true).write(true).truncate(false).mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("failed to open lock anchor {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect lock anchor {}", path.display()))?;
    if !metadata.is_file() {
        bail!("lock anchor {} is not a regular file", path.display());
    }
    Ok(file)
}

fn lock_exclusive_nonblocking(file: &File) -> std::io::Result<()> {
    // SAFETY: flock only observes the descriptor, which `file` keeps open.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// PID holding an exclusive `flock` on `path`, read from `/proc/locks`.
pub fn exclusive_lock_holder_pid(path: &Path) -> Result<Option<u32>> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect lock anchor {}", path.display()))?;
    lock_holder_pid(&metadata, false)
}

/// PID holding any (shared or exclusive) `flock` on `path`. A shared holder
/// also blocks an exclusive acquisition, so contention diagnostics use this.
pub fn flock_holder_pid(path: &Path) -> Result<Option<u32>> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect lock anchor {}", path.display()))?;
    lock_holder_pid(&metadata, true)
}

fn lock_holder_pid(metadata: &fs::Metadata, include_shared: bool) -> Result<Option<u32>> {
    let locks = fs::read_to_string("/proc/locks").context("read kernel lock table")?;
    find_lock_holder(&locks, metadata.dev(), metadata.ino(), include_shared)
}

fn find_lock_holder(locks: &str, dev: u64, ino: u64, include_shared: bool) -> Result<Option<u32>> {
    // Linux dev_t encoding, as printed by /proc/locks as MAJOR:MINOR:INODE.
    let major = ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0x0fff);
    let minor = ((dev >> 12) & 0xffff_ff00) | (dev & 0xff);
    let needle = format!("{major:02x}:{minor:02x}:{ino}");
    let inode_suffix = format!(":{ino}");
    // Some filesystems report a different device in /proc/locks than stat;
    // accept a unique inode-only match in that case.
    let mut inode_match = None;
    let mut ambiguous_inode = false;
    for line in locks.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() < 6
            || parts[1] != "FLOCK"
            || !(parts[3] == "WRITE" || (include_shared && parts[3] == "READ"))
        {
            continue;
        }
        let pid = parts[4].parse::<u32>().context("parse lock holder pid")?;
        if parts[5] == needle {
            return Ok(Some(pid));
        }
        if parts[5].ends_with(&inode_suffix) {
            if inode_match.is_some_and(|candidate| candidate != pid) {
                ambiguous_inode = true;
            }
            inode_match = Some(pid);
        }
    }
    Ok(inode_match.filter(|_| !ambiguous_inode))
}

fn format_holder(pid: u32) -> String {
    let cmdline = fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .replace('\0', " ");
    if cmdline.trim().is_empty() {
        format!("pid {pid}")
    } else {
        format!("pid {pid} ({})", cmdline.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const CHILD_LOCK: &str = "NIX_MANAGER_CORE_TEST_CHILD_LOCK";
    const CHILD_READY: &str = "NIX_MANAGER_CORE_TEST_CHILD_READY";

    /// Children forked by concurrent tests share descriptors until they exec,
    /// so a released lock can stay held briefly.
    fn after_release<T>(mut attempt: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = attempt() {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "lock still held 5s after release"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn read_only_anchor(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("lock");
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        path
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn atomic_write_preserves_existing_mode_and_applies_new_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/out.json");
        atomic_write(&path, b"one", 0o640).unwrap();
        assert_eq!(
            (fs::read(&path).unwrap(), mode(&path)),
            (b"one".to_vec(), 0o640)
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        atomic_write(&path, b"two", 0o600).unwrap();
        assert_eq!(
            (fs::read(&path).unwrap(), mode(&path)),
            (b"two".to_vec(), 0o755)
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn read_only_anchor_serializes_names_holder_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let path = read_only_anchor(dir.path());
        let first = ExclusiveFileLock::acquire(&path, "test operation").unwrap();
        let error =
            ExclusiveFileLock::acquire_wait(&path, "test operation", Duration::from_millis(20))
                .unwrap_err()
                .to_string();
        assert!(error.contains("another test operation"), "{error}");
        assert!(
            error.contains(&format!("pid {}", std::process::id())),
            "{error}"
        );
        assert_eq!(
            exclusive_lock_holder_pid(&path).unwrap(),
            Some(std::process::id())
        );
        assert_eq!(flock_holder_pid(&path).unwrap(), Some(std::process::id()));
        drop(first);
        after_release(|| ExclusiveFileLock::try_acquire(&path).unwrap());
        assert!(path.exists());
    }

    #[test]
    fn inherited_lock_lasts_until_the_last_descriptor_closes() {
        let dir = tempfile::tempdir().unwrap();
        let path = read_only_anchor(dir.path());
        let lock = ExclusiveFileLock::try_acquire(&path).unwrap().unwrap();
        let cloexec = |file: &File| unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC;
        assert_ne!(cloexec(&lock.file), 0);
        lock.inherit_across_exec().unwrap();
        assert_eq!(cloexec(&lock.file), 0);
        let inherited = lock.as_fd().try_clone_to_owned().unwrap();
        drop(lock);
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_none());
        drop(inherited);
        after_release(|| ExclusiveFileLock::try_acquire(&path).unwrap());
    }

    #[test]
    fn dropping_releases_a_lock_shared_with_a_forked_helper() {
        let dir = tempfile::tempdir().unwrap();
        let path = read_only_anchor(dir.path());
        let lock = ExclusiveFileLock::try_acquire(&path).unwrap().unwrap();
        // A helper forked by another thread shares the open file description
        // until it execs, even though the descriptor is close-on-exec.
        let forked = lock.as_fd().try_clone_to_owned().unwrap();
        drop(lock);
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_some());
        drop(forked);
    }

    #[test]
    fn created_anchor_clears_stale_metadata_and_fallback_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let fallback = dir.path().join("fallback");
        fs::write(&fallback, "pid=999999 stale\n").unwrap();
        let trusted = dir.path().join("missing/lock");
        let first = ExclusiveFileLock::acquire_with_fallback(&trusted, &fallback, "op").unwrap();
        assert_eq!(fs::read(&fallback).unwrap(), b"");
        assert!(!trusted.exists());
        assert!(ExclusiveFileLock::acquire_or_create(&fallback, "op").is_err());
        drop(first);
        if unsafe { libc::geteuid() } != 0 {
            // An uninspectable trusted anchor must not divert to the fallback.
            let hidden = dir.path().join("hidden");
            fs::create_dir(&hidden).unwrap();
            fs::write(hidden.join("lock"), []).unwrap();
            fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
            let result =
                ExclusiveFileLock::acquire_with_fallback(&hidden.join("lock"), &fallback, "op");
            fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(result.is_err());
        }
        after_release(|| ExclusiveFileLock::acquire_or_create(&fallback, "op").ok());
    }

    #[test]
    fn symlinked_anchor_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let target = read_only_anchor(dir.path());
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(ExclusiveFileLock::try_acquire(&link).is_err());
        assert!(ExclusiveFileLock::acquire_or_create(&link, "op").is_err());
    }

    #[test]
    fn lock_table_prefers_exact_device_and_rejects_ambiguous_inodes() {
        let dev = (0x103 << 8) | 0x05; // major 0x103, minor 5
        let table = "1: FLOCK  ADVISORY  WRITE 10 00:2a:77 0 EOF\n\
                     2: FLOCK  ADVISORY  READ  11 103:05:77 0 EOF\n\
                     3: POSIX  ADVISORY  WRITE 12 103:05:77 0 EOF\n";
        assert_eq!(find_lock_holder(table, dev, 77, true).unwrap(), Some(11));
        assert_eq!(find_lock_holder(table, dev, 77, false).unwrap(), Some(10));
        let ambiguous = "1: FLOCK ADVISORY WRITE 10 00:2a:77 0 EOF\n2: FLOCK ADVISORY WRITE 20 00:2b:77 0 EOF\n";
        assert_eq!(find_lock_holder(ambiguous, dev, 77, false).unwrap(), None);
    }

    #[test]
    fn signal_releases_lock_without_removing_anchor() {
        if let Some(path) = std::env::var_os(CHILD_LOCK) {
            let _lock = ExclusiveFileLock::acquire(Path::new(&path), "test operation").unwrap();
            fs::write(std::env::var_os(CHILD_READY).unwrap(), []).unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = read_only_anchor(dir.path());
        let ready = dir.path().join("ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "fs::tests::signal_releases_lock_without_removing_anchor",
            ])
            .env(CHILD_LOCK, &path)
            .env(CHILD_READY, &ready)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "lock child exited early"
            );
            assert!(Instant::now() < deadline, "lock child did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_none());
        // SAFETY: the pid is the live child spawned above.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
            0
        );
        assert!(!child.wait().unwrap().success());
        assert!(path.exists());
        after_release(|| ExclusiveFileLock::try_acquire(&path).unwrap());
    }
}
