//! Shared source intent and lock types for managers that reconcile external
//! repositories or release assets.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSource {
    pub repository: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    pub subdirectory: String,
}

fn default_branch() -> String {
    "main".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLock {
    pub repository: String,
    pub branch: String,
    pub revision: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocks {
    #[serde(default)]
    pub git: Vec<GitLock>,
}

impl SourceLocks {
    pub fn find(&self, repository: &str, branch: &str) -> Option<&GitLock> {
        self.git
            .iter()
            .find(|lock| lock.repository == repository && lock.branch == branch)
    }

    pub fn upsert(&mut self, lock: GitLock) {
        if let Some(existing) = self
            .git
            .iter_mut()
            .find(|item| item.repository == lock.repository && item.branch == lock.branch)
        {
            *existing = lock;
        } else {
            self.git.push(lock);
            self.git
                .sort_by(|a, b| (&a.repository, &a.branch).cmp(&(&b.repository, &b.branch)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_a_branch_lock() {
        let mut locks = SourceLocks::default();
        locks.upsert(GitLock {
            repository: "example/repo".into(),
            branch: "main".into(),
            revision: "old".into(),
        });
        locks.upsert(GitLock {
            repository: "example/repo".into(),
            branch: "main".into(),
            revision: "new".into(),
        });
        assert_eq!(locks.git.len(), 1);
        assert_eq!(locks.git[0].revision, "new");
    }
}
