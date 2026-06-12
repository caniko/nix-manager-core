//! Push secrets to forge (Codeberg / GitHub) Actions secret stores.
//!
//! # Auth
//!
//! - **Codeberg**: reads the `fj` auth store at
//!   `${XDG_DATA_HOME:-$HOME/.local/share}/forgejo-cli/keys.json`.
//! - **GitHub**: delegates to the `gh` CLI.

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
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

#[derive(Debug, Deserialize)]
struct FjAuthStore {
    hosts: std::collections::BTreeMap<String, FjHostAuth>,
}

#[derive(Debug, Deserialize)]
struct FjHostAuth {
    token: String,
}

/// Resolve a Codeberg/Forgejo bearer token from the `fj` auth store.
pub fn fj_bearer_token(host: &str) -> Result<String> {
    let path = fj_auth_store_path()?;
    let raw = fs::read_to_string(&path).map_err(|e| {
        anyhow!(
            "could not read fj auth store from {}: {e}\n\
             run `fj auth login --host {host}` or `fj auth add-key <user>` for {host}",
            path.display()
        )
    })?;
    let store: FjAuthStore = serde_json::from_str(&raw).map_err(|e| {
        anyhow!(
            "could not parse fj auth store from {}: {e}\n\
             run `fj auth login --host {host}` or `fj auth add-key <user>` for {host}",
            path.display()
        )
    })?;
    let token = store
        .hosts
        .get(host)
        .ok_or_else(|| {
            anyhow!(
                "fj auth store {} has no token for {host}\n\
                 run `fj auth login --host {host}` or `fj auth add-key <user>` for {host}",
                path.display()
            )
        })?
        .token
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(anyhow!(
            "fj auth store {} has an empty token for {host}\n\
             run `fj auth login --host {host}` or `fj auth add-key <user>` for {host}",
            path.display()
        ));
    }
    Ok(token)
}

/// Resolve a Codeberg/Forgejo bearer token from the `fj` auth store.
pub fn codeberg_bearer_token(host: &str) -> Result<String> {
    fj_bearer_token(host)
}

fn fj_auth_store_path() -> Result<PathBuf> {
    let data_home = match std::env::var_os("XDG_DATA_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                anyhow!("HOME is not set and XDG_DATA_HOME did not point to the fj auth store")
            })?;
            PathBuf::from(home).join(".local/share")
        }
    };
    Ok(data_home.join("forgejo-cli/keys.json"))
}

fn parse_repo(repo: &str) -> Result<(&str, &str)> {
    let (owner, repo_name) = repo
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid repo format `{repo}`: expected `owner/repo`"))?;
    if owner.is_empty() || repo_name.is_empty() || repo_name.contains('/') {
        return Err(anyhow!(
            "invalid repo format `{repo}`: expected `owner/repo`"
        ));
    }
    Ok((owner, repo_name))
}

fn codeberg_client(host: &str, bearer: &str) -> Result<forgejo_api::sync::Forgejo> {
    let base_url = url::Url::parse(&format!("https://{host}"))
        .map_err(|e| anyhow!("invalid host `{host}`: {e}"))?;

    forgejo_api::sync::Forgejo::new(forgejo_api::Auth::Token(bearer), base_url)
        .map_err(|e| anyhow!("failed to create forgejo client for {host}: {e}"))
}

/// Push a secret to a Codeberg/Forgejo repository via its API.
pub fn push_codeberg_secret(host: &str, repo: &str, name: &str, value: &str) -> Result<()> {
    ui::step(format!(
        "codeberg/{host}: setting `{name}` secret on {repo}"
    ));
    let bearer = codeberg_bearer_token(host)?;

    let (owner, repo_name) = parse_repo(repo)?;
    let api = codeberg_client(host, &bearer)?;

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

/// Push a non-secret variable to a Codeberg/Forgejo repository via its API.
pub fn push_codeberg_variable(host: &str, repo: &str, name: &str, value: &str) -> Result<()> {
    ui::step(format!(
        "codeberg/{host}: setting `{name}` variable on {repo}"
    ));
    let bearer = codeberg_bearer_token(host)?;

    let (owner, repo_name) = parse_repo(repo)?;
    let api = codeberg_client(host, &bearer)?;

    let update = api
        .update_repo_variable(
            owner,
            repo_name,
            name,
            forgejo_api::structs::UpdateVariableOption {
                name: None,
                value: value.to_string(),
            },
        )
        .send();

    if let Err(err) = update {
        if !is_not_found(&err) {
            return Err(anyhow!(
                "failed to update variable `{name}` on {host}/{repo}: {err}\n\
                 check that the stored token has `write:repository` scope on {repo}"
            ));
        }

        api.create_repo_variable(
            owner,
            repo_name,
            name,
            forgejo_api::structs::CreateVariableOption {
                value: value.to_string(),
            },
        )
        .send()
        .map_err(|e| {
            anyhow!(
                "failed to create variable `{name}` on {host}/{repo}: {e}\n\
                 check that the stored token has `write:repository` scope on {repo}"
            )
        })?;
    }

    Ok(())
}

/// Push a secret to a Codeberg/Forgejo organization Actions store via its API.
pub fn push_codeberg_organization_secret(
    host: &str,
    org: &str,
    name: &str,
    value: &str,
) -> Result<()> {
    ui::step(format!(
        "codeberg/{host}: setting `{name}` organization secret on {org}"
    ));
    let bearer = codeberg_bearer_token(host)?;
    let api = codeberg_client(host, &bearer)?;

    api.update_org_secret(
        org,
        name,
        forgejo_api::structs::CreateOrUpdateSecretOption {
            data: value.to_string(),
        },
    )
    .send()
    .map_err(|e| {
        anyhow!(
            "failed to set organization secret `{name}` on {host}/{org}: {e}\n\
             check that the stored token can manage organization Actions secrets for {org}"
        )
    })?;

    Ok(())
}

/// Push a non-secret variable to a Codeberg/Forgejo organization via its API.
pub fn push_codeberg_organization_variable(
    host: &str,
    org: &str,
    name: &str,
    value: &str,
) -> Result<()> {
    ui::step(format!(
        "codeberg/{host}: setting `{name}` organization variable on {org}"
    ));
    let bearer = codeberg_bearer_token(host)?;
    let api = codeberg_client(host, &bearer)?;

    let update = api
        .update_org_variable(
            org,
            name,
            forgejo_api::structs::UpdateVariableOption {
                name: None,
                value: value.to_string(),
            },
        )
        .send();

    if let Err(err) = update {
        if !is_not_found(&err) {
            return Err(anyhow!(
                "failed to update organization variable `{name}` on {host}/{org}: {err}\n\
                 check that the stored token can manage organization Actions variables for {org}"
            ));
        }

        api.create_org_variable(
            org,
            name,
            forgejo_api::structs::CreateVariableOption {
                value: value.to_string(),
            },
        )
        .send()
        .map_err(|e| {
            anyhow!(
                "failed to create organization variable `{name}` on {host}/{org}: {e}\n\
                 check that the stored token can manage organization Actions variables for {org}"
            )
        })?;
    }

    Ok(())
}

/// Push a secret to the authenticated user's Codeberg/Forgejo Actions store via its API.
pub fn push_codeberg_user_secret(host: &str, name: &str, value: &str) -> Result<()> {
    ui::step(format!(
        "codeberg/{host}: setting `{name}` user secret on authenticated user"
    ));
    let bearer = codeberg_bearer_token(host)?;
    let api = codeberg_client(host, &bearer)?;

    api.update_user_secret(
        name,
        forgejo_api::structs::CreateOrUpdateSecretOption {
            data: value.to_string(),
        },
    )
    .send()
    .map_err(|e| {
        anyhow!(
            "failed to set user secret `{name}` on {host}: {e}\n\
             check that the stored token can manage authenticated-user Actions secrets"
        )
    })?;

    Ok(())
}

/// Push a non-secret variable to the authenticated user's Codeberg/Forgejo Actions store.
pub fn push_codeberg_user_variable(host: &str, name: &str, value: &str) -> Result<()> {
    ui::step(format!(
        "codeberg/{host}: setting `{name}` user variable on authenticated user"
    ));
    let bearer = codeberg_bearer_token(host)?;
    let api = codeberg_client(host, &bearer)?;

    let update = api
        .update_user_variable(
            name,
            forgejo_api::structs::UpdateVariableOption {
                name: None,
                value: value.to_string(),
            },
        )
        .send();

    if let Err(err) = update {
        if !is_not_found(&err) {
            return Err(anyhow!(
                "failed to update user variable `{name}` on {host}: {err}\n\
                 check that the stored token can manage authenticated-user Actions variables"
            ));
        }

        api.create_user_variable(
            name,
            forgejo_api::structs::CreateVariableOption {
                value: value.to_string(),
            },
        )
        .send()
        .map_err(|e| {
            anyhow!(
                "failed to create user variable `{name}` on {host}: {e}\n\
                 check that the stored token can manage authenticated-user Actions variables"
            )
        })?;
    }

    Ok(())
}

fn is_not_found(err: &forgejo_api::ForgejoError) -> bool {
    matches!(
        err,
        forgejo_api::ForgejoError::ApiError(api)
            if matches!(api.error_kind(), forgejo_api::ApiErrorKind::NotFound { .. })
    )
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
        let old_xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
        std::env::remove_var("CODEBERG_TOKEN");
        std::env::remove_var("XDG_DATA_HOME");
        let result = f();
        match old_token {
            Some(v) => std::env::set_var("CODEBERG_TOKEN", v),
            None => std::env::remove_var("CODEBERG_TOKEN"),
        }
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_xdg_data_home {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        result
    }

    fn write_fj_auth_store(data_home: &std::path::Path, host: &str, token: &str) {
        let auth_dir = data_home.join("forgejo-cli");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(
            auth_dir.join("keys.json"),
            format!(
                r#"{{
                  "hosts": {{
                    "{host}": {{
                      "type": "oauth",
                      "name": "caniko",
                      "token": "{token}",
                      "refresh_token": "unused",
                      "expires_at": [2026, 161, 6, 16, 47, 492946905, 0, 0, 0]
                    }}
                  }},
                  "aliases": {{}},
                  "default_ssh": []
                }}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn codeberg_bearer_token_reads_xdg_fj_auth_store() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            write_fj_auth_store(dir.path(), "codeberg.org", "fj-token-value");

            std::env::set_var("XDG_DATA_HOME", dir.path());

            let result = codeberg_bearer_token("codeberg.org").unwrap();
            assert_eq!(result, "fj-token-value");
        });
    }

    #[test]
    fn codeberg_bearer_token_falls_back_to_home_fj_auth_store() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            write_fj_auth_store(
                &dir.path().join(".local/share"),
                "codeberg.org",
                "home-fj-token-value",
            );
            std::env::set_var("HOME", dir.path());

            let result = codeberg_bearer_token("codeberg.org").unwrap();
            assert_eq!(result, "home-fj-token-value");
        });
    }

    #[test]
    fn codeberg_bearer_token_errors_when_host_missing() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            write_fj_auth_store(dir.path(), "git.example.test", "fj-token-value");
            std::env::set_var("XDG_DATA_HOME", dir.path());

            let result = codeberg_bearer_token("codeberg.org");
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("no token for codeberg.org"));
            assert!(err.contains("fj auth login --host codeberg.org"));
        });
    }

    #[test]
    fn codeberg_bearer_token_errors_when_token_empty() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            write_fj_auth_store(dir.path(), "codeberg.org", "   ");
            std::env::set_var("XDG_DATA_HOME", dir.path());

            let result = codeberg_bearer_token("codeberg.org");
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("empty token for codeberg.org"));
            assert!(err.contains("fj auth add-key <user>"));
        });
    }

    #[test]
    fn codeberg_bearer_token_ignores_codeberg_token_env() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            write_fj_auth_store(dir.path(), "codeberg.org", "fj-token-value");
            std::env::set_var("XDG_DATA_HOME", dir.path());
            std::env::set_var("CODEBERG_TOKEN", "env-token-value");

            let result = codeberg_bearer_token("codeberg.org").unwrap();
            assert_eq!(result, "fj-token-value");
        });
    }

    #[test]
    fn codeberg_bearer_token_ignores_old_host_token_file() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            let token_dir = dir.path().join(".local/share/forgejo-cli/codeberg.org");
            fs::create_dir_all(&token_dir).unwrap();
            fs::write(token_dir.join("TOKEN"), "old-file-token-value\n").unwrap();
            write_fj_auth_store(
                &dir.path().join(".local/share"),
                "codeberg.org",
                "fj-token-value",
            );
            std::env::set_var("HOME", dir.path());

            let result = codeberg_bearer_token("codeberg.org").unwrap();
            assert_eq!(result, "fj-token-value");
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
            assert!(err.contains("fj auth login --host codeberg.org"));
        });
    }

    #[test]
    fn parse_repo_accepts_owner_repo_only() {
        assert_eq!(
            parse_repo("caniko/rs-modde").unwrap(),
            ("caniko", "rs-modde")
        );
        assert!(parse_repo("caniko").is_err());
        assert!(parse_repo("caniko/").is_err());
        assert!(parse_repo("/rs-modde").is_err());
        assert!(parse_repo("caniko/rs-modde/extra").is_err());
    }

    #[test]
    fn not_found_detection_matches_forgejo_api_kind() {
        let err = forgejo_api::ForgejoError::ApiError(
            forgejo_api::ApiErrorKind::NotFound { errors: None }.into(),
        );
        assert!(is_not_found(&err));

        let err = forgejo_api::ForgejoError::ApiError(forgejo_api::ApiErrorKind::Forbidden.into());
        assert!(!is_not_found(&err));
    }
}
