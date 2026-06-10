//! Push secrets to forge (Codeberg / GitHub) Actions secret stores.
//!
//! # Auth
//!
//! - **Codeberg**: `$CODEBERG_TOKEN` env var, falling back to the forgejo-cli
//!   token file at `~/.local/share/forgejo-cli/<host>/TOKEN`.
//! - **GitHub**: delegates to the `gh` CLI.

use anyhow::{anyhow, Result};
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::ui;

const DEFAULT_CODEBERG_HOST: &str = "codeberg.org";

/// Push a secret to a GitHub repository via `gh secret set`.
pub fn push_github_secret(repo: &str, name: &str, value: &str) -> Result<()> {
    ui::step(format!("github: setting `{name}` secret on {repo}"));
    let mut child = Command::new("gh")
        .args(["secret", "set", name, "--repo", repo])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow!("failed to spawn gh: {e}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("gh stdin was not captured"))?;
        stdin.write_all(value.as_bytes())?;
    }
    drop(child.stdin.take());
    let status = child.wait().map_err(|e| anyhow!("wait on gh: {e}"))?;
    if !status.success() {
        return Err(anyhow!(
            "gh secret set --repo {repo} exited with status {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".into())
        ));
    }
    Ok(())
}

/// Resolve a Codeberg/Forgejo bearer token from the environment or the
/// forgejo-cli token file.
pub fn codeberg_bearer_token(host: &str) -> Result<String> {
    if let Ok(tok) = std::env::var("CODEBERG_TOKEN") {
        let tok = tok.trim().to_string();
        if !tok.is_empty() {
            return Ok(tok);
        }
    }
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME is not set"))?;
    let path = format!("{home}/.local/share/forgejo-cli/{host}/TOKEN");
    fs::read_to_string(&path)
        .map(|s| s.trim().to_string())
        .map_err(|e| {
            anyhow!(
                "could not read forge token from {path}: {e}\n\
                 set CODEBERG_TOKEN or run `forgejo-cli auth login` against {host}"
            )
        })
}

/// Push a secret to a Codeberg/Forgejo repository via its API.
pub fn push_codeberg_secret(host: &str, repo: &str, name: &str, value: &str) -> Result<()> {
    ui::step(format!(
        "codeberg/{host}: setting `{name}` secret on {repo}"
    ));
    let bearer = codeberg_bearer_token(host)?;

    let (owner, repo_name) = repo
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid repo format `{repo}`: expected `owner/repo`"))?;

    let base_url = url::Url::parse(&format!("https://{host}"))
        .map_err(|e| anyhow!("invalid host `{host}`: {e}"))?;

    let api = forgejo_api::sync::Forgejo::new(
        forgejo_api::Auth::Token(&bearer),
        base_url,
    )
    .map_err(|e| anyhow!("failed to create forgejo client for {host}: {e}"))?;

    api.update_repo_secret(
        owner,
        repo_name,
        name,
        forgejo_api::structs::CreateOrUpdateSecretOption {
            data: value.to_string(),
        },
    )
    .send()
    .map_err(|e| {
        anyhow!(
            "failed to set secret `{name}` on {host}/{repo}: {e}\n\
             check that the stored token has `write:repository` scope on {repo}"
        )
    })?;

    Ok(())
}

/// Convenience wrapper around [`push_codeberg_secret`] targeting codeberg.org.
pub fn push_codeberg_org_secret(repo: &str, name: &str, value: &str) -> Result<()> {
    push_codeberg_secret(DEFAULT_CODEBERG_HOST, repo, name, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_env<T>(f: impl FnOnce() -> T) -> T {
        let _g = ENV_LOCK.lock().unwrap();
        let old_token = std::env::var("CODEBERG_TOKEN").ok();
        let old_home = std::env::var("HOME").ok();
        std::env::remove_var("CODEBERG_TOKEN");
        let result = f();
        // Restore original env
        match old_token {
            Some(v) => std::env::set_var("CODEBERG_TOKEN", v),
            None => std::env::remove_var("CODEBERG_TOKEN"),
        }
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    #[test]
    fn codeberg_bearer_token_uses_env_var() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            std::env::set_var("CODEBERG_TOKEN", "env-token-value");
            std::env::set_var("HOME", dir.path());

            let result = codeberg_bearer_token("codeberg.org").unwrap();
            assert_eq!(result, "env-token-value");
        });
    }

    #[test]
    fn codeberg_bearer_token_env_var_wins_over_file() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            let token_dir = dir.path().join(".local/share/forgejo-cli/codeberg.org");
            fs::create_dir_all(&token_dir).unwrap();
            fs::write(token_dir.join("TOKEN"), "file-token-value\n").unwrap();

            std::env::set_var("CODEBERG_TOKEN", "env-token-value");
            std::env::set_var("HOME", dir.path());

            let result = codeberg_bearer_token("codeberg.org").unwrap();
            assert_eq!(result, "env-token-value");
        });
    }

    #[test]
    fn codeberg_bearer_token_falls_back_to_file() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            let token_dir = dir.path().join(".local/share/forgejo-cli/codeberg.org");
            fs::create_dir_all(&token_dir).unwrap();
            fs::write(token_dir.join("TOKEN"), "file-token-value\n").unwrap();
            std::env::set_var("HOME", dir.path());

            let result = codeberg_bearer_token("codeberg.org").unwrap();
            assert_eq!(result, "file-token-value");
        });
    }

    #[test]
    fn codeberg_bearer_token_errors_when_unresolvable() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            std::env::set_var("HOME", dir.path());

            let result = codeberg_bearer_token("codeberg.org");
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("CODEBERG_TOKEN") || err.contains("forgejo-cli"));
        });
    }
}
