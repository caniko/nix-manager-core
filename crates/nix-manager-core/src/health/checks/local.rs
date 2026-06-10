use serde_json::json;
use std::path::PathBuf;

use crate::exec::cap;
use crate::health::{Check, CheckContext, CheckResult};

pub struct CommandPresent {
    pub name: &'static str,
    pub commands: Vec<(&'static str, &'static str)>,
}

impl CommandPresent {
    pub fn new(commands: Vec<(&'static str, &'static str)>) -> Self {
        Self {
            name: "tooling",
            commands,
        }
    }
}

impl Check for CommandPresent {
    fn run(&self, _ctx: &mut CheckContext) -> CheckResult {
        let missing: Vec<_> = self
            .commands
            .iter()
            .filter(|(c, _)| which(c).is_none())
            .collect();
        if missing.is_empty() {
            CheckResult::pass(self.name, "all required commands present").with_details(json!({
                "commands": self.commands.iter().map(|(c, _)| *c).collect::<Vec<_>>()
            }))
        } else {
            let names: Vec<&str> = missing.iter().map(|(c, _)| *c).collect();
            CheckResult::fail(
                self.name,
                format!("missing required commands: {}", names.join(", ")),
            )
            .with_details(json!({
                "missing": missing.iter()
                    .map(|(c, r)| json!({ "cmd": c, "reason": r }))
                    .collect::<Vec<_>>(),
            }))
        }
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub struct Sudo {
    pub name: &'static str,
}

impl Default for Sudo {
    fn default() -> Self {
        Self { name: "sudo" }
    }
}

impl Check for Sudo {
    fn run(&self, ctx: &mut CheckContext) -> CheckResult {
        let probe = cap("sudo", ["-n", "true"]);
        let ok = probe.ok();
        ctx.set_fact("can_sudo", ok);
        if ok {
            CheckResult::pass(self.name, "passwordless sudo available")
        } else {
            CheckResult::warn(
                self.name,
                "passwordless sudo not available; sudo-gated checks will be skipped",
            )
            .with_details(json!({ "stderr": probe.stderr.trim() }))
        }
    }
}
