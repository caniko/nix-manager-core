use serde_json::json;
use std::path::{Path, PathBuf};

use crate::exec::cap;
use crate::health::{Check, CheckContext, CheckResult};

pub struct FileSecret {
    pub name: &'static str,
    pub path: PathBuf,
    pub label: String,
    pub fail_on_missing: bool,
}

impl FileSecret {
    pub fn new(name: &'static str, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let label = path.display().to_string();
        Self {
            name,
            path,
            label,
            fail_on_missing: true,
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn warn_on_missing(mut self) -> Self {
        self.fail_on_missing = false;
        self
    }
}

impl Check for FileSecret {
    fn run(&self, _ctx: &mut CheckContext) -> CheckResult {
        if !self.path.exists() {
            let msg = format!("{} ({}) is missing", self.label, self.path.display());
            let details = json!({ "path": self.path.display().to_string(), "reason": "missing" });
            return if self.fail_on_missing {
                CheckResult::fail(self.name, msg).with_details(details)
            } else {
                CheckResult::warn(self.name, msg).with_details(details)
            };
        }
        let stat = cap(
            "stat",
            ["-c", "%U:%G %a %s", &self.path.display().to_string()],
        );
        if !stat.ok() {
            return CheckResult::fail(
                self.name,
                format!(
                    "stat {} failed: {}",
                    self.path.display(),
                    stat.stderr.trim()
                ),
            );
        }
        let mut parts = stat.stdout.trim().splitn(3, ' ');
        let owner = parts.next().unwrap_or_default().to_string();
        let mode = parts.next().unwrap_or_default().to_string();
        let size: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let mode_ok = matches!(mode.as_str(), "400" | "440" | "600" | "640");
        let details = json!({
            "path": self.path.display().to_string(),
            "owner": owner,
            "mode": mode,
            "size": size,
        });
        if size == 0 {
            return CheckResult::fail(self.name, format!("{} is empty", self.label))
                .with_details(details);
        }
        if !mode_ok {
            return CheckResult::warn(
                self.name,
                format!(
                    "{} mode={mode} too permissive (expected 400/440/600/640)",
                    self.label
                ),
            )
            .with_details(details);
        }
        CheckResult::pass(
            self.name,
            format!("{} present, non-empty, tightly permissioned", self.label),
        )
        .with_details(details)
    }
}

pub struct StoragePath {
    pub name: &'static str,
    pub path: PathBuf,
    pub expected_owner: Option<String>,
    pub min_free_bytes: u64,
    pub min_free_inodes: u64,
}

impl StoragePath {
    pub fn new(name: &'static str, path: impl Into<PathBuf>) -> Self {
        Self {
            name,
            path: path.into(),
            expected_owner: None,
            min_free_bytes: 20 * 1024 * 1024 * 1024,
            min_free_inodes: 100_000,
        }
    }

    pub fn expected_owner(mut self, owner: impl Into<String>) -> Self {
        self.expected_owner = Some(owner.into());
        self
    }

    pub fn min_free_bytes(mut self, b: u64) -> Self {
        self.min_free_bytes = b;
        self
    }
}

impl Check for StoragePath {
    fn run(&self, _ctx: &mut CheckContext) -> CheckResult {
        if !self.path.exists() {
            return CheckResult::fail(
                self.name,
                format!("storage path {} does not exist", self.path.display()),
            )
            .with_details(json!({ "path": self.path.display().to_string() }));
        }
        let p = self.path.display().to_string();
        let mnt = cap(
            "findmnt",
            [
                "-T",
                &p,
                "--noheadings",
                "--output",
                "TARGET,SOURCE,FSTYPE,OPTIONS",
            ],
        );
        let stat = cap("stat", ["-c", "%U:%G %a", &p]);
        let df_bytes = cap(
            "df",
            ["-B1", "--output=source,size,used,avail,pcent,target", &p],
        );
        let df_inodes = cap("df", ["-i", "--output=iavail,target", &p]);

        let owner_mode = stat.stdout.trim().to_string();
        let mount_line = mnt.stdout.trim().to_string();
        let avail_bytes: u64 = df_bytes
            .stdout
            .lines()
            .nth(1)
            .and_then(|line| line.split_whitespace().nth(3))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let pcent: String = df_bytes
            .stdout
            .lines()
            .nth(1)
            .and_then(|line| line.split_whitespace().nth(4))
            .unwrap_or("?")
            .to_string();
        let avail_inodes: u64 = df_inodes
            .stdout
            .lines()
            .nth(1)
            .and_then(|line| line.split_whitespace().next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let avail_gb = avail_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
        let details = json!({
            "path": p,
            "mount": mount_line,
            "ownerMode": owner_mode,
            "availableBytes": avail_bytes,
            "availableGB": (avail_gb * 10.0).round() / 10.0,
            "percentUsed": pcent,
            "availableInodes": avail_inodes,
        });

        let mut problems: Vec<String> = Vec::new();
        if mount_line.is_empty() {
            problems.push("path is not on a recognized mountpoint".to_string());
        }
        if let Some(want) = self.expected_owner.as_ref().filter(|w| !owner_mode.starts_with(*w)) {
            problems.push(format!(
                "ownership is {owner_mode}, expected {want} (mode digits may follow)"
            ));
        }
        if avail_bytes < self.min_free_bytes {
            problems.push(format!(
                "only {:.1} GB free (threshold: {:.1} GB)",
                avail_gb,
                self.min_free_bytes as f64 / 1024.0 / 1024.0 / 1024.0
            ));
        }
        if avail_inodes < self.min_free_inodes {
            problems.push(format!(
                "only {avail_inodes} inodes free (threshold: {})",
                self.min_free_inodes
            ));
        }

        if problems.is_empty() {
            CheckResult::pass(
                self.name,
                format!(
                    "{} healthy ({:.1} GB free, {avail_inodes} inodes free)",
                    p, avail_gb
                ),
            )
            .with_details(details)
        } else {
            CheckResult::warn(self.name, problems.join(" | ")).with_details(details)
        }
    }
}

pub struct ZeroByteFileScan {
    pub name: &'static str,
    pub roots: Vec<PathBuf>,
    pub extensions: Vec<&'static str>,
    pub head_lines: usize,
}

impl ZeroByteFileScan {
    pub fn new(name: &'static str, roots: Vec<PathBuf>, extensions: Vec<&'static str>) -> Self {
        Self {
            name,
            roots,
            extensions,
            head_lines: 50,
        }
    }
}

impl Check for ZeroByteFileScan {
    fn run(&self, _ctx: &mut CheckContext) -> CheckResult {
        if self.roots.is_empty() {
            return CheckResult::skip(self.name, "no scan roots provided");
        }
        let mut offenders: Vec<String> = Vec::new();
        for root in &self.roots {
            walk_for_empty(root, &self.extensions, &mut offenders);
        }
        let scanned = self.roots.len();
        let details = json!({
            "scannedClosures": scanned,
            "offenderCount": offenders.len(),
            "offendersHead": offenders.iter().take(self.head_lines).cloned().collect::<Vec<_>>(),
        });
        if offenders.is_empty() {
            CheckResult::pass(
                self.name,
                format!(
                    "scanned {scanned} root(s); no zero-byte files of any tracked extension found"
                ),
            )
            .with_details(details)
        } else {
            CheckResult::fail(
                self.name,
                format!(
                    "found {} zero-byte file(s) matching tracked extensions — these will re-poison the cache if pushed",
                    offenders.len()
                ),
            )
            .with_details(details)
        }
    }
}

fn walk_for_empty(dir: &Path, extensions: &[&str], out: &mut Vec<String>) {
    let walker = match std::fs::read_dir(dir) {
        Ok(w) => w,
        Err(_) => return,
    };
    for entry in walker.flatten() {
        let p = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            walk_for_empty(&p, extensions, out);
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let name = match p.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !extensions.iter().any(|ext| name.ends_with(ext)) {
            continue;
        }
        let len = match entry.metadata() {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if len == 0 {
            out.push(p.display().to_string());
        }
    }
}
