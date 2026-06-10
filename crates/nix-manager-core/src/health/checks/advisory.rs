use serde_json::Value;

use crate::health::{Check, CheckContext, CheckResult, Status};

pub struct ConstantAdvisory {
    pub name: &'static str,
    pub status: Status,
    pub message: String,
    pub details: Value,
}

impl Check for ConstantAdvisory {
    fn run(&self, _ctx: &mut CheckContext) -> CheckResult {
        CheckResult {
            name: self.name,
            status: self.status,
            message: self.message.clone(),
            details: self.details.clone(),
        }
    }
}
