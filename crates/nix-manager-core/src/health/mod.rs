//! Generic health-check framework: define a `Check`, push it into a `Suite`,
//! run the suite to get an ordered list of `CheckResult`s, then render or exit
//! on the aggregate verdict.
//!
//! The framework is intentionally domain-agnostic. Concrete reusable checks
//! (systemd unit, TCP listener, HTTP probe, file secret, storage path, ...)
//! live in `crate::health::checks`. Domain-specific compositions
//! (e.g. attic-rescue-readiness) build a `Suite` of those checks in the
//! relevant command module.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub mod checks;

/// The four states a single check can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Skip,
    Warn,
    Fail,
}

impl Status {
    /// Higher = worse. Used to compute the suite-wide verdict.
    pub fn rank(self) -> u8 {
        match self {
            Status::Pass => 0,
            Status::Skip => 1,
            Status::Warn => 2,
            Status::Fail => 3,
        }
    }

    pub fn worst_of<'a>(items: impl IntoIterator<Item = &'a Status>) -> Status {
        items
            .into_iter()
            .copied()
            .max_by_key(|s| s.rank())
            .unwrap_or(Status::Pass)
    }

    pub fn icon(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Skip => "SKIP",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

/// A single check's outcome. `name` is short and stable (used as a key in
/// JSON output); `message` is the one-line human-readable explanation;
/// `details` is an arbitrary structured blob for machine consumers.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: &'static str,
    pub status: Status,
    pub message: String,
    #[serde(default)]
    pub details: Value,
}

impl CheckResult {
    pub fn pass(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Pass,
            message: message.into(),
            details: Value::Null,
        }
    }
    pub fn warn(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Warn,
            message: message.into(),
            details: Value::Null,
        }
    }
    pub fn fail(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            message: message.into(),
            details: Value::Null,
        }
    }
    pub fn skip(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Skip,
            message: message.into(),
            details: Value::Null,
        }
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

/// Shared state carried across all checks in a single suite run.
///
/// Use `set_fact` to surface state for downstream checks — e.g. the sudo
/// check records `can_sudo=true` so later checks can short-circuit when it's
/// false.
#[derive(Debug, Default)]
pub struct CheckContext {
    pub facts: BTreeMap<&'static str, Value>,
}

impl CheckContext {
    pub fn set_fact(&mut self, key: &'static str, value: impl Into<Value>) {
        self.facts.insert(key, value.into());
    }
    pub fn fact_bool(&self, key: &str) -> Option<bool> {
        self.facts.get(key).and_then(Value::as_bool)
    }
}

/// Anything runnable by a `Suite`. Checks return a result rather than
/// propagating errors — internal failures are encoded as `Status::Fail` so the
/// suite never gets short-circuited by an `anyhow::Error` partway through.
pub trait Check: Send {
    fn run(&self, ctx: &mut CheckContext) -> CheckResult;
}

/// Ordered list of (check, halt-on-fail) entries.
#[derive(Default)]
pub struct Suite {
    entries: Vec<Entry>,
}

struct Entry {
    check: Box<dyn Check>,
    halt_on_fail: bool,
}

impl Suite {
    pub fn new() -> Self {
        Self::default()
    }
    /// Append a check that does not abort the suite on FAIL.
    pub fn add(&mut self, check: impl Check + 'static) -> &mut Self {
        self.entries.push(Entry {
            check: Box::new(check),
            halt_on_fail: false,
        });
        self
    }
    /// Append a check that aborts the suite on FAIL. Useful for preflight
    /// gates (host identity, tool availability) where continuing would just
    /// produce nonsense results.
    pub fn add_halting(&mut self, check: impl Check + 'static) -> &mut Self {
        self.entries.push(Entry {
            check: Box::new(check),
            halt_on_fail: true,
        });
        self
    }
    pub fn run(self, ctx: &mut CheckContext) -> Vec<CheckResult> {
        let mut out = Vec::with_capacity(self.entries.len());
        for entry in self.entries {
            let result = entry.check.run(ctx);
            let halt_here = entry.halt_on_fail && result.status == Status::Fail;
            out.push(result);
            if halt_here {
                break;
            }
        }
        out
    }
}

/// Aggregate verdict over a list of results.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub overall: Status,
    pub ready: bool,
    pub blocked: bool,
    pub results: Vec<CheckResult>,
}

impl Report {
    pub fn from(results: Vec<CheckResult>) -> Self {
        let overall = Status::worst_of(results.iter().map(|r| &r.status));
        Self {
            overall,
            ready: !matches!(overall, Status::Fail),
            blocked: matches!(overall, Status::Fail),
            results,
        }
    }
    /// 0 if nothing failed (and, with `strict`, nothing warned). 1 otherwise.
    pub fn exit_code(&self, strict: bool) -> i32 {
        let bad = self
            .results
            .iter()
            .any(|r| r.status == Status::Fail || (strict && r.status == Status::Warn));
        if bad {
            1
        } else {
            0
        }
    }
    /// One-line tagline matching the nu script's verdict copy.
    pub fn verdict(&self) -> &'static str {
        match self.overall {
            Status::Fail => "BLOCKED — resolve every FAIL row above before proceeding.",
            Status::Warn => "PROCEED WITH CARE — review every WARN row above.",
            Status::Skip | Status::Pass => "READY — every check passed.",
        }
    }
}

/// Render a report as a human-readable plaintext block to stdout.
pub fn render_human(report: &Report, title: &str) {
    println!("=== {} — overall: {} ===", title, report.overall.icon());
    println!();
    for r in &report.results {
        println!("[{}] {}: {}", r.status.icon(), r.name, r.message);
    }
    println!();
    println!("Result: {}", report.verdict());
}

/// Render a report as pretty-printed JSON to stdout.
pub fn render_json(report: &Report) -> Result<()> {
    serde_json::to_writer_pretty(std::io::stdout().lock(), report)?;
    println!();
    Ok(())
}
