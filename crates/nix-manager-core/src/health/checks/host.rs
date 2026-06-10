use serde_json::json;

use crate::exec::local_hostname;
use crate::health::{Check, CheckContext, CheckResult};

pub struct HostIdentity {
    pub name: &'static str,
    pub expected: String,
    pub force: bool,
}

impl HostIdentity {
    pub fn new(expected: impl Into<String>) -> Self {
        Self {
            name: "identity",
            expected: expected.into(),
            force: false,
        }
    }

    pub fn force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }
}

impl Check for HostIdentity {
    fn run(&self, ctx: &mut CheckContext) -> CheckResult {
        let actual = local_hostname().unwrap_or_default();
        let details = json!({ "hostname": actual, "expected": self.expected });
        ctx.set_fact("hostname", actual.clone());
        if actual == self.expected {
            CheckResult::pass(self.name, format!("running on expected host ({actual})"))
                .with_details(details)
        } else if self.force {
            CheckResult::warn(
                self.name,
                format!(
                    "hostname is {actual}, expected {} — running anyway (--force-host)",
                    self.expected
                ),
            )
            .with_details(details)
        } else {
            CheckResult::fail(
                self.name,
                format!(
                    "must run on host {}, got {actual}. Re-run with --force-host to override.",
                    self.expected
                ),
            )
            .with_details(details)
        }
    }
}
