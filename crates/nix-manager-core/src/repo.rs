//! Repository root discovery and update-lock primitives.
//!
//! Walks upward from the current directory looking for a marker file
//! (defaults to `flake.nix`) so commands can operate from any subdirectory
//! without hard-coding paths.
//!
//! The [`with_repo_update_lock`] function serialises repo-write operations
//! (lockfile bumps, git index writes) via an OS-level exclusive flock so that
//! concurrent invocations do not corrupt shared state.

use anyhow::{anyhow, Context, Result};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::ui;

/// Walk upward from the current directory looking for `marker` (default
/// `flake.nix`). Returns the first directory that contains one. Errors if
/// no marker is found before reaching the filesystem root.
pub fn find_root(marker: &str) -> Result<PathBuf> {
    let start = env::current_dir().map_err(|e| anyhow!("read current directory: {e}"))?;
    let mut cur = start.clone();
    loop {
        if cur.join(marker).is_file() {
            return Ok(cur);
        }
        if !cur.pop() {
            break;
        }
    }
    Err(anyhow!(
        "no '{marker}' found in {} or any parent",
        start.display()
    ))
}

pub fn find_flake_root() -> Result<PathBuf> {
    find_root("flake.nix")
}

/// Serialize repo update operations that can write `flake.lock` or git index
/// state. The OS releases this lock automatically if the process exits.
pub fn with_repo_update_lock<T>(reason: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let root = env::current_dir().context("read current directory for repo update lock")?;
    let path = lock_path_for_repo_root(&root)?;
    let _guard = acquire_lock_at(&path, reason)?;
    f()
}

fn lock_path_for_repo_root(root: &Path) -> Result<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Ok(dot_git.join("repo-update.lock"));
    }

    if dot_git.is_file() {
        let raw = std::fs::read_to_string(&dot_git)
            .with_context(|| format!("read {}", dot_git.display()))?;
        let gitdir = raw
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .ok_or_else(|| anyhow!("{} is not a gitdir file", dot_git.display()))?;
        let path = PathBuf::from(gitdir);
        let gitdir = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        return Ok(gitdir.join("repo-update.lock"));
    }

    Ok(root.join(".repo-update.lock"))
}

fn acquire_lock_at(path: &Path, reason: &str) -> Result<RepoUpdateLockGuard> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create lock directory {}", parent.display()))?;
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open repo update lock {}", path.display()))?;

    match try_lock_exclusive(&file) {
        Ok(()) => {
            write_lock_holder(&mut file, reason)?;
            Ok(RepoUpdateLockGuard { file })
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock => {
            let holder = std::fs::read_to_string(path).unwrap_or_else(|_| "unknown".to_string());
            let timeout = env::var("CANIX_REPO_LOCK_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or(Duration::from_secs(300));
            ui::warn(format!(
                "waiting up to {}s for repo update lock {} ({reason}); holder: {}",
                timeout.as_secs(),
                path.display(),
                holder.trim()
            ));
            let started = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(250));
                match try_lock_exclusive(&file) {
                    Ok(()) => break,
                    Err(error)
                        if error.kind() == ErrorKind::WouldBlock && started.elapsed() < timeout => {
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        return Err(anyhow!(
                            "timed out after {}s waiting for repo update lock {}; holder: {}",
                            timeout.as_secs(),
                            path.display(),
                            holder.trim()
                        ));
                    }
                    Err(error) => {
                        return Err(error).with_context(|| format!("lock {}", path.display()))
                    }
                }
            }
            write_lock_holder(&mut file, reason)?;
            Ok(RepoUpdateLockGuard { file })
        }
        Err(e) => Err(e).with_context(|| format!("lock {}", path.display())),
    }
}

fn write_lock_holder(file: &mut File, reason: &str) -> Result<()> {
    file.set_len(0)?;
    file.rewind()?;
    let cmdline = std::fs::read_to_string(format!("/proc/{}/cmdline", std::process::id()))
        .unwrap_or_default()
        .replace('\0', " ");
    writeln!(
        file,
        "pid={} reason={} cmdline={}",
        std::process::id(),
        reason,
        cmdline.trim()
    )?;
    file.sync_data()?;
    Ok(())
}

fn try_lock_exclusive(file: &File) -> std::io::Result<()> {
    flock(file, LOCK_EX | LOCK_NB)
}

fn unlock(file: &File) -> std::io::Result<()> {
    flock(file, LOCK_UN)
}

fn flock(file: &File, operation: c_int) -> std::io::Result<()> {
    // SAFETY: `flock` only observes the raw file descriptor and operation
    // flags. `file` remains open for the duration of the call.
    let rc = unsafe { c_flock(file.as_raw_fd(), operation) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

const LOCK_EX: c_int = 2;
const LOCK_NB: c_int = 4;
const LOCK_UN: c_int = 8;

unsafe extern "C" {
    #[link_name = "flock"]
    fn c_flock(fd: c_int, operation: c_int) -> c_int;
}

struct RepoUpdateLockGuard {
    file: File,
}

impl Drop for RepoUpdateLockGuard {
    fn drop(&mut self) {
        let _ = unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    // ------------------------------------------------------------------
    //  find_root / find_flake_root
    // ------------------------------------------------------------------

    #[test]
    fn finds_marker_in_subdirectory() {
        let _g = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        fs::write(repo.join("flake.nix"), "{}").unwrap();
        let nested = repo.join("docs").join("planning").join("xyz");
        fs::create_dir_all(&nested).unwrap();

        let prev = env::current_dir().unwrap();
        env::set_current_dir(&nested).unwrap();
        let result = find_root("flake.nix");
        env::set_current_dir(&prev).unwrap();

        assert_eq!(
            result.unwrap().canonicalize().unwrap(),
            repo.canonicalize().unwrap()
        );
    }

    #[test]
    fn finds_custom_marker() {
        let _g = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        fs::write(repo.join("CUSTOM.lock"), "data").unwrap();
        let deep = repo.join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();

        let prev = env::current_dir().unwrap();
        env::set_current_dir(&deep).unwrap();
        let result = find_root("CUSTOM.lock");
        env::set_current_dir(&prev).unwrap();

        assert_eq!(
            result.unwrap().canonicalize().unwrap(),
            repo.canonicalize().unwrap()
        );
    }

    #[test]
    fn errors_outside_any_marker() {
        let _g = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev = env::current_dir().unwrap();
        env::set_current_dir(tmp.path()).unwrap();
        let result = find_root("flake.nix");
        env::set_current_dir(&prev).unwrap();
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    //  repo update lock
    // ------------------------------------------------------------------

    #[test]
    fn lock_path_uses_git_dir_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let git = tmp.path().join(".git");
        fs::create_dir(&git).unwrap();

        assert_eq!(
            lock_path_for_repo_root(tmp.path()).unwrap(),
            git.join("repo-update.lock")
        );
    }

    #[test]
    fn lock_path_resolves_gitdir_file_relative_to_repo() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("../actual-git")).unwrap();
        fs::write(tmp.path().join(".git"), "gitdir: ../actual-git\n").unwrap();

        assert_eq!(
            lock_path_for_repo_root(tmp.path()).unwrap(),
            tmp.path().join("../actual-git/repo-update.lock")
        );
    }

    #[test]
    fn exclusive_lock_blocks_second_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("repo-update.lock");
        let _guard = acquire_lock_at(&path, "test").unwrap();

        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        assert!(try_lock_exclusive(&second).is_err());
    }
}
