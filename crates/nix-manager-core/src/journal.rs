use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;

/// A single journal entry as emitted by `journalctl --output=json`.
#[derive(Debug, Clone, Deserialize)]
pub struct RawLogEntry {
    #[serde(rename = "__REALTIME_TIMESTAMP")]
    pub realtime_timestamp: Option<String>,
    #[serde(rename = "MESSAGE")]
    pub message: Option<String>,
    #[serde(rename = "SYSLOG_IDENTIFIER")]
    pub syslog_identifier: Option<String>,
    #[serde(rename = "_PID")]
    pub pid: Option<String>,
    #[serde(rename = "_SYSTEMD_UNIT")]
    pub systemd_unit: Option<String>,
    #[serde(rename = "PRIORITY")]
    pub priority: Option<String>,
    #[serde(rename = "_HOSTNAME")]
    pub hostname: Option<String>,
    #[serde(rename = "_COMM")]
    pub comm: Option<String>,
    /// Catch any other fields.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Parse a single line of journalctl JSON output.
pub fn parse_journal_json(line: &str) -> Result<RawLogEntry> {
    serde_json::from_str(line).with_context(|| format!("parsing journal line: {line:.80}"))
}

/// Collect journal entries from a local host.
///
/// `unit` filters to a specific systemd unit; pass `None` for all units.
/// `since` is a journalctl time expression (e.g. "24h ago", "2024-01-01").
pub fn collect_local(unit: Option<&str>, since: Option<&str>, lines: Option<usize>) -> Result<Vec<RawLogEntry>> {
    let mut args = vec![
        "--output=json".to_string(),
        "--no-pager".to_string(),
    ];
    if let Some(u) = unit {
        args.push("-u".to_string());
        args.push(u.to_string());
    }
    if let Some(s) = since {
        args.push("--since".to_string());
        args.push(s.to_string());
    }
    if let Some(n) = lines {
        args.push("-n".to_string());
        args.push(n.to_string());
    }

    let output = Command::new("journalctl")
        .args(&args)
        .output()
        .context("spawning journalctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("journalctl failed (exit: {:?}): {}", output.status.code(), stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(parse_journal_json)
        .collect()
}

/// Follow journal entries on a local host, printing each line.
pub fn follow_local(unit: Option<&str>, lines: Option<usize>) -> Result<()> {
    let mut args = vec![
        "--output=json".to_string(),
        "--follow".to_string(),
    ];
    if let Some(u) = unit {
        args.push("-u".to_string());
        args.push(u.to_string());
    }
    if let Some(n) = lines {
        args.push("-n".to_string());
        args.push(n.to_string());
    }

    let status = Command::new("journalctl")
        .args(&args)
        .status()
        .context("spawning journalctl --follow")?;

    if !status.success() {
        anyhow::bail!("journalctl --follow failed (exit: {:?})", status.code());
    }
    Ok(())
}
