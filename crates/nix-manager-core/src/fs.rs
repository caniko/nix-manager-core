use anyhow::{Context, Result, anyhow};
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

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
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("tmp")
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
