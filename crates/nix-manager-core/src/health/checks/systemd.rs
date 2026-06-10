use serde_json::json;
use std::collections::BTreeMap;

use crate::exec::cap;
use crate::health::{Check, CheckContext, CheckResult};

pub struct SystemdUnit {
    pub name: &'static str,
    pub unit: String,
    pub require_enabled: bool,
}

impl SystemdUnit {
    pub fn new(name: &'static str, unit: impl Into<String>) -> Self {
        Self {
            name,
            unit: unit.into(),
            require_enabled: true,
        }
    }
}

impl Check for SystemdUnit {
    fn run(&self, _ctx: &mut CheckContext) -> CheckResult {
        let active = cap("systemctl", ["is-active", &self.unit]);
        let enabled = cap("systemctl", ["is-enabled", &self.unit]);
        let show = cap(
            "systemctl",
            [
                "show",
                &self.unit,
                "--property=ActiveState,SubState,Result,NRestarts,ExecMainStartTimestamp",
            ],
        );
        let log = cap(
            "journalctl",
            [
                "-u",
                &self.unit,
                "-p",
                "err..alert",
                "-n",
                "20",
                "--no-pager",
                "-o",
                "short-iso",
            ],
        );

        let show_map = parse_show(&show.stdout);
        let sub_state = show_map.get("SubState").cloned().unwrap_or_default();
        let result_state = show_map.get("Result").cloned().unwrap_or_default();
        let restarts: i64 = show_map
            .get("NRestarts")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let active_s = active.stdout.trim().to_string();
        let enabled_s = enabled.stdout.trim().to_string();

        let log_lines: Vec<&str> = log
            .stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("--"))
            .collect();

        let details = json!({
            "active": active_s,
            "enabled": enabled_s,
            "subState": sub_state,
            "result": result_state,
            "restartCount": restarts,
            "startedAt": show_map.get("ExecMainStartTimestamp").cloned().unwrap_or_default(),
            "recentErrorCount": log_lines.len(),
            "recentErrorTail": log_lines.iter().rev().take(5).rev().cloned().collect::<Vec<_>>(),
        });

        if active_s != "active" || sub_state != "running" || result_state != "success" {
            return CheckResult::fail(
                self.name,
                format!(
                    "{} not healthy (active={active_s}, sub={sub_state}, result={result_state})",
                    self.unit
                ),
            )
            .with_details(details);
        }
        if self.require_enabled && enabled_s != "enabled" {
            return CheckResult::warn(
                self.name,
                format!(
                    "{} is active but not enabled at boot ({enabled_s})",
                    self.unit
                ),
            )
            .with_details(details);
        }
        if restarts > 0 {
            return CheckResult::warn(
                self.name,
                format!("{} has restarted {restarts} times since boot", self.unit),
            )
            .with_details(details);
        }
        if !log_lines.is_empty() {
            return CheckResult::warn(
                self.name,
                format!(
                    "{} is active but emitted {} recent errors — review journalctl -u {}",
                    self.unit,
                    log_lines.len(),
                    self.unit
                ),
            )
            .with_details(details);
        }
        CheckResult::pass(self.name, format!("{} active, enabled, clean", self.unit))
            .with_details(details)
    }
}

fn parse_show(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
