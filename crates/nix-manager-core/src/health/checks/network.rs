use serde_json::json;

use crate::exec::cap;
use crate::health::{Check, CheckContext, CheckResult};

pub struct TcpListener {
    pub name: &'static str,
    pub port: u16,
}

impl TcpListener {
    pub fn new(name: &'static str, port: u16) -> Self {
        Self { name, port }
    }
}

impl Check for TcpListener {
    fn run(&self, _ctx: &mut CheckContext) -> CheckResult {
        let filter = format!("sport = :{}", self.port);
        let r = cap("ss", ["-Hltn", &filter]);
        let needle = format!(":{}", self.port);
        let listeners: Vec<&str> = r.stdout.lines().filter(|l| l.contains(&needle)).collect();
        if listeners.is_empty() {
            CheckResult::fail(
                self.name,
                format!(
                    "nothing listening on TCP {} — service is not accepting connections",
                    self.port
                ),
            )
            .with_details(json!({ "ss_output": r.stdout }))
        } else {
            CheckResult::pass(self.name, format!("listening on TCP {}", self.port))
                .with_details(json!({ "listeners": listeners }))
        }
    }
}

pub struct HttpProbe {
    pub name: &'static str,
    pub url: String,
    pub timeout_seconds: u64,
}

impl HttpProbe {
    pub fn new(name: &'static str, url: impl Into<String>) -> Self {
        Self {
            name,
            url: url.into(),
            timeout_seconds: 5,
        }
    }
}

impl Check for HttpProbe {
    fn run(&self, _ctx: &mut CheckContext) -> CheckResult {
        let timeout = self.timeout_seconds.to_string();
        let r = cap(
            "curl",
            [
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code} %{time_total}",
                "--max-time",
                &timeout,
                &self.url,
            ],
        );
        if !r.ok() {
            return CheckResult::fail(
                self.name,
                format!("HTTP probe to {} failed at the transport layer", self.url),
            )
            .with_details(json!({ "stderr": r.stderr.trim(), "exitCode": r.exit_code }));
        }
        let mut parts = r.stdout.split_whitespace();
        let code = parts.next().unwrap_or("000").to_string();
        let elapsed = parts.next().unwrap_or("?").to_string();
        let numeric: i32 = code.parse().unwrap_or(0);
        let details = json!({ "statusCode": code, "elapsedSeconds": elapsed });
        if numeric >= 500 {
            CheckResult::fail(
                self.name,
                format!("HTTP {code} from {} — server-side error", self.url),
            )
            .with_details(details)
        } else {
            CheckResult::pass(
                self.name,
                format!("HTTP {code} from {} in {elapsed}s", self.url),
            )
            .with_details(details)
        }
    }
}
