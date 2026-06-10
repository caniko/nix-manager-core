use anyhow::{Result, bail};

use crate::exec;

/// Resolve the current login user via `id -un`. Rejects empty and `root`.
///
/// `context` is a human-readable label (e.g. "a home-manager secret") that
/// appears in error messages so the user knows what operation requires a
/// non-root login user.
pub fn current_login_user(context: &str) -> Result<String> {
    let user = exec::capture("id", ["-un"])?;
    let user = user.trim();
    if user.is_empty() {
        bail!("`id -un` returned an empty username; pass --user <USER>");
    }
    if user == "root" {
        bail!(
            "current login user resolved to `root`; refusing to store {context} under root home-manager. Run this as the HM user or pass --user <USER>."
        );
    }
    Ok(user.to_string())
}
