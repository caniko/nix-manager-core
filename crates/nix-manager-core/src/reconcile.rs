//! Generic declarative reconciliation primitives for auxiliary managers.
//!
//! Domain projects provide typed resources and the side effects for applying
//! them. This module owns the common lifecycle and stable machine-readable
//! change representation so managers can expose the same `check`, `plan`, and
//! `apply` contract without coupling their schemas together.

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ResourceId(pub String);

impl ResourceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl std::fmt::Display for ResourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    Create,
    Update,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub resource: ResourceId,
    pub kind: ChangeKind,
    pub summary: String,
}

impl Change {
    pub fn new(resource: impl Into<String>, kind: ChangeKind, summary: impl Into<String>) -> Self {
        Self {
            resource: ResourceId::new(resource),
            kind,
            summary: summary.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub changes: Vec<Change>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn extend(&mut self, changes: impl IntoIterator<Item = Change>) {
        self.changes.extend(changes);
    }
}

/// A resource supplies its desired-state diff and applies one of its own
/// changes. The trait is deliberately small so domain crates can implement it
/// for files, databases, game registrations, or external source checkouts.
pub trait Resource {
    fn id(&self) -> ResourceId;
    fn plan(&self) -> Result<Vec<Change>>;
    fn apply(&self, change: &Change) -> Result<()>;
}

pub fn plan(resources: &[&dyn Resource]) -> Result<Plan> {
    let mut result = Plan::default();
    for resource in resources {
        result.extend(resource.plan()?);
    }
    result.changes.sort_by(|a, b| a.resource.cmp(&b.resource));
    Ok(result)
}

pub fn apply(resources: &[&dyn Resource], expected: &Plan) -> Result<()> {
    for change in &expected.changes {
        let resource = resources
            .iter()
            .find(|resource| resource.id() == change.resource)
            .ok_or_else(|| {
                anyhow::anyhow!("resource {} disappeared before apply", change.resource)
            })?;
        resource.apply(change)?;
    }
    Ok(())
}

pub fn render_json(plan: &Plan) -> Result<String> {
    Ok(serde_json::to_string_pretty(plan)? + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_are_sorted_by_resource_id() {
        let first = TestResource("z");
        let second = TestResource("a");
        let plan = plan(&[&first, &second]).expect("plan");
        assert_eq!(plan.changes[0].resource.0, "a");
        assert_eq!(plan.changes[1].resource.0, "z");
    }

    struct TestResource(&'static str);

    impl Resource for TestResource {
        fn id(&self) -> ResourceId {
            ResourceId::new(self.0)
        }
        fn plan(&self) -> Result<Vec<Change>> {
            Ok(vec![Change::new(self.0, ChangeKind::Update, "test")])
        }
        fn apply(&self, _change: &Change) -> Result<()> {
            Ok(())
        }
    }
}
