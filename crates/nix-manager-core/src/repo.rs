//! Repository root discovery.
//!
//! Walks upward from the current directory looking for a marker file
//! (defaults to `flake.nix`) so commands can operate from any subdirectory
//! without hard-coding paths.

use anyhow::{Result, anyhow};
use std::env;
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    static CWD_LOCK: Mutex<()> = Mutex::new(());

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
}
