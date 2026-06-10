use anyhow::{Result, anyhow, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::ui;

pub const RESULTS_ROOT: &str = ".nix-results";

/// Walk `.` for files with a `.nix` extension whose body contains `marker`.
/// `marker` is matched as a literal substring (no regex). Unreadable files are
/// silently skipped. Symlinks are followed by `walkdir`'s default.
pub fn find_nix_files_containing(marker: &str) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(".") {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("nix") {
            continue;
        }
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if raw.contains(marker) {
            out.push(path.to_path_buf());
        }
    }
    Ok(out)
}

pub fn host_system() -> Result<&'static str> {
    let arch = Command::new("uname")
        .arg("-m")
        .output()
        .map_err(|e| anyhow!("uname -m failed: {e}"))?;
    let arch = String::from_utf8_lossy(&arch.stdout).trim().to_string();
    Ok(match arch.as_str() {
        "x86_64" => "x86_64-linux",
        "aarch64" | "arm64" => "aarch64-linux",
        other => bail!("unsupported host architecture: {other}"),
    })
}

pub fn results_dir() -> Result<PathBuf> {
    let dir = PathBuf::from(RESULTS_ROOT);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn result_link_path(name: &str) -> Result<PathBuf> {
    if name.contains('/') {
        bail!("result link names must not contain '/': {name}");
    }
    Ok(results_dir()?.join(name))
}

pub fn resolve_attr(attr: &str) -> Result<String> {
    if attr.contains("{system}") {
        Ok(attr.replace("{system}", host_system()?))
    } else {
        Ok(attr.to_string())
    }
}

/// Run `nix build` against the resolved attribute. When `name` is `Some`,
/// keep a stable symlink at `.nix-results/<name>` pointing at the output.
/// Returns the produced store path.
pub fn nix_build(attr: &str, name: Option<&str>) -> Result<PathBuf> {
    let resolved = resolve_attr(attr)?;
    ui::header(format!("Building {resolved}"));

    let pb = ui::spinner(format!("nix build {resolved}"));
    let mut cmd = Command::new("nix");
    cmd.arg("build")
        .arg(&resolved)
        .arg("--print-out-paths")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let link_path = match name {
        Some(n) => {
            let path = result_link_path(n)?;
            cmd.arg("--out-link").arg(&path);
            Some(path)
        }
        None => {
            cmd.arg("--no-link");
            None
        }
    };

    let output = cmd.output().map_err(|e| {
        pb.finish_and_clear();
        anyhow!("failed to spawn nix: {e}")
    })?;

    if !output.status.success() {
        ui::fail_spinner(pb, "nix build failed");
        bail!("nix build exited with {}", output.status);
    }

    let out_path = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("nix build produced no output path"))?
        .to_string();

    ui::finish_spinner(pb, format!("Built {resolved}"));

    if let Some(link) = &link_path {
        ui::step_success(format!("Linked {} → {}", link.display(), out_path));
    }

    Ok(PathBuf::from(out_path))
}

/// Remove `.nix-results/*` and any legacy `result*` symlinks at the repo root.
/// Returns the number of links removed.
pub fn clean_results() -> Result<usize> {
    let mut removed = 0usize;

    let root = PathBuf::from(RESULTS_ROOT);
    if root.exists() {
        ui::header(format!("Cleaning {RESULTS_ROOT}/"));
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            std::fs::remove_file(&path).or_else(|_| std::fs::remove_dir_all(&path))?;
            ui::step(format!("removed {}", path.display()));
            removed += 1;
        }
    } else {
        ui::header(format!("{RESULTS_ROOT}/ does not exist — skipping"));
    }

    let mut legacy: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(".")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("result") {
            continue;
        }
        let metadata = entry.path().symlink_metadata()?;
        if metadata.file_type().is_symlink() {
            legacy.push(entry.path());
        }
    }

    if legacy.is_empty() {
        ui::info("no legacy repo-root result symlinks found");
    } else {
        ui::header("Removing legacy repo-root result symlinks");
        for link in &legacy {
            std::fs::remove_file(link)?;
            ui::step(format!("removed {}", link.display()));
            removed += 1;
        }
    }

    ui::success(format!("Done — {removed} link(s) removed"));
    Ok(removed)
}
