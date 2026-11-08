//! Push secrets to forge (Codeberg / GitHub) Actions secret stores.
//!
//! # Auth
//!
//! - **Codeberg**: reads the `fj` auth store at
//!   `${XDG_DATA_HOME:-$HOME/.local/share}/forgejo-cli/keys.json`.
//! - **GitHub**: delegates to the `gh` CLI.

use anyhow::{anyhow, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::ui;

pub mod gpg;

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

#[derive(Debug, Clone, Deserialize)]
struct FjHostAuth {
    #[serde(rename = "type")]
    auth_type: String,
    #[serde(default)]
    name: String,
    token: String,
    refresh_token: Option<String>,
    expires_at: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodebergAuthKind {
    ApplicationToken,
    OAuth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodebergAuth {
    username: String,
    token: String,
    kind: CodebergAuthKind,
}

impl CodebergAuth {
    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn forgejo_auth(&self) -> forgejo_api::Auth<'_> {
        match self.kind {
            CodebergAuthKind::ApplicationToken => forgejo_api::Auth::Token(&self.token),
            CodebergAuthKind::OAuth => forgejo_api::Auth::OAuth2(&self.token),
        }
    }
}

fn fj_auth_login_command(host: &str) -> String {
    format!("fj -H {host} auth login")
}

fn fj_auth_add_token_command(host: &str) -> String {
    format!("fj -H {host} auth add-token")
}

/// Resolve Codeberg/Forgejo authentication from the `fj` auth store.
pub fn codeberg_auth(host: &str) -> Result<CodebergAuth> {
    let path = fj_auth_store_path()?;
    let mut auth = load_fj_host_auth(&path, host)?;
    let mut refresh_failure = None;
    if auth.needs_refresh(Utc::now()) {
        // `fj` refreshes expired OAuth tokens itself and persists them to its store, so any
        // authenticated call is enough; we never touch the store or the tokens.
        match refresh_with_fj(host, &auth) {
            Ok(()) => auth = load_fj_host_auth(&path, host)?,
            Err(RefreshError::FjMissing) => {}
            Err(RefreshError::Failed(cause)) => refresh_failure = Some(cause),
        }
    }
    auth.validate(host, &path)
        .map_err(|e| match refresh_failure {
            Some(cause) => anyhow!("{e}\nfj token refresh failed: {cause}"),
            None => e,
        })
}

fn load_fj_host_auth(path: &std::path::Path, host: &str) -> Result<FjHostAuth> {
    let raw = fs::read_to_string(path).map_err(|e| {
        anyhow!(
            "could not read fj auth store from {}: {e}\n\
             run `{}` or `{}` for {host}",
            path.display(),
            fj_auth_login_command(host),
            fj_auth_add_token_command(host)
        )
    })?;
    let mut store: FjAuthStore = serde_json::from_str(&raw).map_err(|e| {
        anyhow!(
            "could not parse fj auth store from {}: {e}\n\
             run `{}` or `{}` for {host}",
            path.display(),
            fj_auth_login_command(host),
            fj_auth_add_token_command(host)
        )
    })?;
    store.hosts.remove(host).ok_or_else(|| {
        anyhow!(
            "fj auth store {} has no token for {host}\n\
                 run `{}` or `{}` for {host}",
            path.display(),
            fj_auth_login_command(host),
            fj_auth_add_token_command(host)
        )
    })
}

enum RefreshError {
    FjMissing,
    Failed(String),
}

/// Upper bound for the `fj whoami` refresh round trip.
const FJ_REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Run `fj -H <host> whoami` so fj refreshes and persists an expired OAuth token. Output is
/// captured and redacted; no secret is passed on the command line.
fn refresh_with_fj(host: &str, auth: &FjHostAuth) -> std::result::Result<(), RefreshError> {
    let secrets = [Some(auth.token.as_str()), auth.refresh_token.as_deref()];
    let redact = |s: &str| {
        let mut out = s.trim().to_string();
        for secret in secrets.iter().flatten().filter(|s| !s.trim().is_empty()) {
            out = out.replace(secret.trim(), "<redacted>");
        }
        out.chars().take(400).collect::<String>()
    };
    let mut child = match Command::new("fj")
        .args(["-H", host, "whoami"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(RefreshError::FjMissing),
        Err(e) => return Err(RefreshError::Failed(format!("could not run fj: {e}"))),
    };
    let deadline = std::time::Instant::now() + FJ_REFRESH_TIMEOUT;
    while child
        .try_wait()
        .map_err(|e| RefreshError::Failed(format!("wait on fj: {e}")))?
        .is_none()
    {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RefreshError::Failed(format!(
                "`fj -H {host} whoami` timed out after {}s",
                FJ_REFRESH_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child
        .wait_with_output()
        .map_err(|e| RefreshError::Failed(format!("collect fj output: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = redact(&String::from_utf8_lossy(&out.stderr));
    Err(RefreshError::Failed(format!(
        "`fj -H {host} whoami` exited with {}{}",
        out.status,
        if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        }
    )))
}

impl FjHostAuth {
    /// An OAuth entry whose expiry has passed and that carries a refresh token.
    fn needs_refresh(&self, now: DateTime<Utc>) -> bool {
        self.auth_type.trim().eq_ignore_ascii_case("oauth")
            && self
                .refresh_token
                .as_deref()
                .is_some_and(|t| !t.trim().is_empty())
            && parse_fj_expires_at(self.expires_at.as_deref(), "", std::path::Path::new(""))
                .is_ok_and(|expires_at| expires_at <= now)
    }

    fn validate(self, host: &str, path: &std::path::Path) -> Result<CodebergAuth> {
        self.validate_at(host, path, Utc::now())
    }

    fn validate_at(
        self,
        host: &str,
        path: &std::path::Path,
        now: DateTime<Utc>,
    ) -> Result<CodebergAuth> {
        let token = self.token.trim().to_string();
        let username = self.name.trim().to_string();

        if token.is_empty() {
            return Err(anyhow!(
                "fj auth store {} has an empty token for {host}\n\
                 run `{}` or `{}` for {host}",
                path.display(),
                fj_auth_login_command(host),
                fj_auth_add_token_command(host)
            ));
        }

        let kind = match self.auth_type.trim().to_ascii_lowercase().as_str() {
            "oauth" => {
                let expires_at = parse_fj_expires_at(self.expires_at.as_deref(), host, path)?;
                if expires_at <= now {
                    let refresh_state = if self
                        .refresh_token
                        .as_deref()
                        .is_some_and(|refresh_token| !refresh_token.trim().is_empty())
                    {
                        "the fj store has a refresh token, but secret-manager does not refresh OAuth tokens itself"
                    } else {
                        "the fj store has no refresh token"
                    };
                    return Err(anyhow!(
                        "fj OAuth token for {host} expired at {expires_at}\n\
                         fj auth store still has {host} as OAuth\n\
                         {refresh_state}\n\
                         run `{}` to refresh it, \
                         or run `{}` to store an application token for unattended secret sync",
                        fj_auth_login_command(host),
                        fj_auth_add_token_command(host)
                    ));
                }
                CodebergAuthKind::OAuth
            }
            "token" | "application" | "applicationtoken" | "application_token" | "key" => {
                CodebergAuthKind::ApplicationToken
            }
            other => {
                return Err(anyhow!(
                    "fj auth store {} has unsupported auth type `{}` for {host}\n\
                     run `{}` for unattended secret sync, \
                     or refresh the OAuth entry with `{}`",
                    path.display(),
                    if other.is_empty() { "<empty>" } else { other },
                    fj_auth_add_token_command(host),
                    fj_auth_login_command(host)
                ));
            }
        };

        Ok(CodebergAuth {
            username,
            token,
            kind,
        })
    }
}

fn parse_fj_expires_at(
    raw: Option<&[serde_json::Value]>,
    host: &str,
    path: &std::path::Path,
) -> Result<DateTime<Utc>> {
    let fields = raw.ok_or_else(|| {
        anyhow!(
            "fj OAuth entry in {} has no expires_at for {host}\n\
             run `{}` to refresh it, \
             or `{}` for unattended secret sync",
            path.display(),
            fj_auth_login_command(host),
            fj_auth_add_token_command(host)
        )
    })?;
    if fields.len() < 6 {
        return Err(anyhow!(
            "fj OAuth entry in {} has malformed expires_at for {host}: expected at least 6 fields, got {}\n\
             run `{}` to refresh it",
            path.display(),
            fields.len(),
            fj_auth_login_command(host)
        ));
    }

    let get_i32 = |idx: usize, name: &str| -> Result<i32> {
        let value = fields[idx].as_i64().ok_or_else(|| {
            anyhow!(
                "fj OAuth entry in {} has non-integer expires_at field `{name}` for {host}\n\
                 run `{}` to refresh it",
                path.display(),
                fj_auth_login_command(host)
            )
        })?;
        i32::try_from(value).map_err(|_| {
            anyhow!(
                "fj OAuth entry in {} has out-of-range expires_at field `{name}` for {host}\n\
                 run `{}` to refresh it",
                path.display(),
                fj_auth_login_command(host)
            )
        })
    };
    let get_u32 = |idx: usize, name: &str| -> Result<u32> {
        let value = fields[idx].as_u64().ok_or_else(|| {
            anyhow!(
                "fj OAuth entry in {} has non-integer expires_at field `{name}` for {host}\n\
                 run `{}` to refresh it",
                path.display(),
                fj_auth_login_command(host)
            )
        })?;
        u32::try_from(value).map_err(|_| {
            anyhow!(
                "fj OAuth entry in {} has out-of-range expires_at field `{name}` for {host}\n\
                 run `{}` to refresh it",
                path.display(),
                fj_auth_login_command(host)
            )
        })
    };

    let year = get_i32(0, "year")?;
    let ordinal = get_u32(1, "ordinal")?;
    let hour = get_u32(2, "hour")?;
    let minute = get_u32(3, "minute")?;
    let second = get_u32(4, "second")?;
    let nanos = get_u32(5, "nanosecond")?;

    let date = NaiveDate::from_yo_opt(year, ordinal).ok_or_else(|| {
        anyhow!(
            "fj OAuth entry in {} has invalid expires_at date for {host}\n\
             run `{}` to refresh it",
            path.display(),
            fj_auth_login_command(host)
        )
    })?;
    let datetime = date
        .and_hms_nano_opt(hour, minute, second, nanos)
        .ok_or_else(|| {
            anyhow!(
                "fj OAuth entry in {} has invalid expires_at time for {host}\n\
             run `{}` to refresh it",
                path.display(),
                fj_auth_login_command(host)
            )
        })?;

    Ok(DateTime::<Utc>::from_naive_utc_and_offset(datetime, Utc))
}

/// Resolve a Codeberg/Forgejo bearer token from the `fj` auth store.
pub fn fj_bearer_token(host: &str) -> Result<String> {
    Ok(codeberg_auth(host)?.token)
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

fn codeberg_client(host: &str, auth: &CodebergAuth) -> Result<forgejo_api::sync::Forgejo> {
    let base_url = url::Url::parse(&format!("https://{host}"))
        .map_err(|e| anyhow!("invalid host `{host}`: {e}"))?;

    forgejo_api::sync::Forgejo::new(auth.forgejo_auth(), base_url)
        .map_err(|e| anyhow!("failed to create forgejo client for {host}: {e}"))
}

fn require_authenticated_user(host: &str, api: &forgejo_api::sync::Forgejo) -> Result<String> {
    let user = api.user_get_current().send().map_err(|e| {
        anyhow!(
            "failed to authenticate to {host}: {e}\n\
             run `{}` for unattended secret sync, \
             or refresh the OAuth entry with `{}`",
            fj_auth_add_token_command(host),
            fj_auth_login_command(host)
        )
    })?;
    let login = user.login.unwrap_or_default();
    if login.trim().is_empty() {
        return Err(anyhow!(
            "authenticated user response from {host} did not include a login\n\
             run `{}` for unattended secret sync",
            fj_auth_add_token_command(host)
        ));
    }
    Ok(login)
}

/// Push a secret to a Codeberg/Forgejo repository via its API.
pub fn push_codeberg_secret(host: &str, repo: &str, name: &str, value: &str) -> Result<()> {
    ui::step(format!(
        "codeberg/{host}: setting `{name}` secret on {repo}"
    ));
    let auth = codeberg_auth(host)?;

    let (owner, repo_name) = parse_repo(repo)?;
    let api = codeberg_client(host, &auth)?;

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
    let auth = codeberg_auth(host)?;

    let (owner, repo_name) = parse_repo(repo)?;
    let api = codeberg_client(host, &auth)?;

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
    let auth = codeberg_auth(host)?;
    let api = codeberg_client(host, &auth)?;

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
    let auth = codeberg_auth(host)?;
    let api = codeberg_client(host, &auth)?;

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
    let auth = codeberg_auth(host)?;
    let api = codeberg_client(host, &auth)?;
    let login = require_authenticated_user(host, &api)?;
    ui::step(format!("codeberg/{host}: authenticated as {login}"));

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
    let auth = codeberg_auth(host)?;
    let api = codeberg_client(host, &auth)?;
    let login = require_authenticated_user(host, &api)?;
    ui::step(format!("codeberg/{host}: authenticated as {login}"));

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
        let old_path = std::env::var_os("PATH");
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
        match old_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        result
    }

    fn write_fj_auth_store(data_home: &std::path::Path, host: &str, token: &str) {
        write_fj_auth_store_with(data_home, host, "OAuth", Some("caniko"), token);
    }

    fn write_fj_auth_store_with(
        data_home: &std::path::Path,
        host: &str,
        auth_type: &str,
        name: Option<&str>,
        token: &str,
    ) {
        let auth_dir = data_home.join("forgejo-cli");
        fs::create_dir_all(&auth_dir).unwrap();
        let name = name
            .map(|name| format!(r#""name": "{name}","#))
            .unwrap_or_default();
        fs::write(
            auth_dir.join("keys.json"),
            format!(
                r#"{{
                  "hosts": {{
                    "{host}": {{
                      "type": "{auth_type}",
                      {name}
                      "token": "{token}",
                      "refresh_token": "unused",
                      "expires_at": [2999, 161, 6, 16, 47, 492946905, 0, 0, 0]
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
    fn codeberg_auth_parses_oauth_fj_entry() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            write_fj_auth_store(dir.path(), "codeberg.org", "oauth-token");
            std::env::set_var("XDG_DATA_HOME", dir.path());

            let auth = codeberg_auth("codeberg.org").unwrap();
            assert_eq!(auth.username(), "caniko");
            assert_eq!(auth.token(), "oauth-token");
            assert_eq!(auth.kind, CodebergAuthKind::OAuth);
        });
    }

    #[test]
    fn codeberg_auth_parses_fj_06_application_without_name() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            write_fj_auth_store_with(dir.path(), "codeberg.org", "Application", None, "app-token");
            std::env::set_var("XDG_DATA_HOME", dir.path());

            let auth = codeberg_auth("codeberg.org").unwrap();
            assert_eq!(auth.token(), "app-token");
            assert_eq!(auth.kind, CodebergAuthKind::ApplicationToken);
        });
    }

    #[test]
    fn codeberg_auth_parses_fj_05_token_with_name() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            write_fj_auth_store_with(
                dir.path(),
                "codeberg.org",
                "Token",
                Some("caniko"),
                "app-token",
            );
            std::env::set_var("XDG_DATA_HOME", dir.path());

            let auth = codeberg_auth("codeberg.org").unwrap();
            assert_eq!(auth.username(), "caniko");
            assert_eq!(auth.token(), "app-token");
            assert_eq!(auth.kind, CodebergAuthKind::ApplicationToken);
        });
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
            assert!(err.contains("fj -H codeberg.org auth login"));
            assert!(err.contains("fj -H codeberg.org auth add-token"));
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
            assert!(err.contains("fj -H codeberg.org auth login"));
            assert!(err.contains("fj -H codeberg.org auth add-token"));
        });
    }

    #[test]
    fn codeberg_auth_errors_when_type_unsupported() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            write_fj_auth_store_with(dir.path(), "codeberg.org", "Session", None, "token");
            std::env::set_var("XDG_DATA_HOME", dir.path());

            let result = codeberg_auth("codeberg.org");
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("unsupported auth type `session`"));
            assert!(err.contains("fj -H codeberg.org auth add-token"));
            assert!(err.contains("fj -H codeberg.org auth login"));
        });
    }

    #[test]
    fn codeberg_auth_errors_when_oauth_token_expired() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            let auth_dir = dir.path().join("forgejo-cli");
            fs::create_dir_all(&auth_dir).unwrap();
            fs::write(
                auth_dir.join("keys.json"),
                r#"{
                  "hosts": {
                    "codeberg.org": {
                      "type": "OAuth",
                      "name": "caniko",
                      "token": "expired-token",
                      "refresh_token": "refresh-token",
                      "expires_at": [2000, 1, 0, 0, 0, 0, 0, 0, 0]
                    }
                  },
                  "aliases": {},
                  "default_ssh": []
                }"#,
            )
            .unwrap();
            let no_fj = tempfile::tempdir().unwrap();
            std::env::set_var("PATH", no_fj.path());
            std::env::set_var("XDG_DATA_HOME", dir.path());

            let result = codeberg_auth("codeberg.org");
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("OAuth token for codeberg.org expired"));
            assert!(err.contains("fj auth store still has codeberg.org as OAuth"));
            assert!(err.contains("does not refresh OAuth tokens itself"));
            assert!(err.contains("fj -H codeberg.org auth login"));
            assert!(err.contains("fj -H codeberg.org auth add-token"));
        });
    }

    const EXPIRED: &str = "[2000, 1, 0, 0, 0, 0, 0, 0, 0]";

    fn keys_json(auth_type: &str, token: &str, refresh: Option<&str>, expires: &str) -> String {
        let refresh = refresh
            .map(|r| format!(r#""refresh_token": "{r}","#))
            .unwrap_or_default();
        format!(
            r#"{{"hosts": {{"codeberg.org": {{"type": "{auth_type}", "name": "caniko",
            "token": "{token}", {refresh} "expires_at": {expires}}},
            "other.example": {{"type": "Application", "token": "other-token"}}}},
            "aliases": {{}}, "default_ssh": []}}"#
        )
    }

    /// Install a fake `fj` first on PATH. It records its argv in `<dir>/args` and runs `body`.
    fn fake_fj(body: &str) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fj");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$@\" > \"{}/args\"\n{body}\n",
                dir.path().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let old = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{old}", dir.path().display()));
        dir
    }

    fn write_keys(data_home: &std::path::Path, json: &str) -> PathBuf {
        let path = data_home.join("forgejo-cli/keys.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, json).unwrap();
        std::env::set_var("XDG_DATA_HOME", data_home);
        path
    }

    #[test]
    fn expired_oauth_is_refreshed_through_fj() {
        with_clean_env(|| {
            let data = tempfile::tempdir().unwrap();
            let keys = write_keys(
                data.path(),
                &keys_json("OAuth", "old-token", Some("refresh-token"), EXPIRED),
            );
            let fresh = data.path().join("fresh.json");
            fs::write(
                &fresh,
                keys_json(
                    "OAuth",
                    "fresh-token",
                    Some("refresh-2"),
                    "[2999, 161, 6, 16, 47, 0, 0, 0, 0]",
                ),
            )
            .unwrap();
            let fj = fake_fj(&format!(
                "cp \"{}\" \"{}\"",
                fresh.display(),
                keys.display()
            ));

            let auth = codeberg_auth("codeberg.org").unwrap();
            assert_eq!(auth.token(), "fresh-token");
            assert_eq!(auth.kind, CodebergAuthKind::OAuth);
            let args = fs::read_to_string(fj.path().join("args")).unwrap();
            assert_eq!(args.trim(), "-H codeberg.org whoami");
        });
    }

    #[test]
    fn expired_oauth_without_refresh_token_keeps_old_error_and_skips_fj() {
        with_clean_env(|| {
            let data = tempfile::tempdir().unwrap();
            write_keys(data.path(), &keys_json("OAuth", "old-token", None, EXPIRED));
            let fj = fake_fj("exit 0");

            let err = codeberg_auth("codeberg.org").unwrap_err().to_string();
            assert!(err.contains("OAuth token for codeberg.org expired"));
            assert!(err.contains("the fj store has no refresh token"));
            assert!(err.contains("fj -H codeberg.org auth login"));
            assert!(!fj.path().join("args").exists());
        });
    }

    #[test]
    fn failed_fj_refresh_reports_cause_without_leaking_tokens() {
        with_clean_env(|| {
            let data = tempfile::tempdir().unwrap();
            write_keys(
                data.path(),
                &keys_json("OAuth", "old-token", Some("refresh-token"), EXPIRED),
            );
            let _fj =
                fake_fj("echo \"invalid_grant for refresh-token (was old-token)\" >&2; exit 3");

            let err = codeberg_auth("codeberg.org").unwrap_err().to_string();
            assert!(err.contains("OAuth token for codeberg.org expired"));
            assert!(err.contains("fj token refresh failed"));
            assert!(err.contains("invalid_grant"));
            assert!(err.contains("<redacted>"));
            assert!(!err.contains("refresh-token (") && !err.contains("old-token"));
        });
    }

    #[test]
    fn application_token_never_invokes_fj() {
        with_clean_env(|| {
            let data = tempfile::tempdir().unwrap();
            let json = keys_json("Application", "app-token", None, EXPIRED);
            let keys = write_keys(data.path(), &json);
            let fj = fake_fj("exit 1");

            let auth = codeberg_auth("codeberg.org").unwrap();
            assert_eq!(auth.token(), "app-token");
            assert_eq!(auth.kind, CodebergAuthKind::ApplicationToken);
            assert!(!fj.path().join("args").exists());
            assert_eq!(fs::read_to_string(keys).unwrap(), json);
        });
    }

    #[test]
    fn codeberg_auth_errors_when_oauth_expiry_missing() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            let auth_dir = dir.path().join("forgejo-cli");
            fs::create_dir_all(&auth_dir).unwrap();
            fs::write(
                auth_dir.join("keys.json"),
                r#"{
                  "hosts": {
                    "codeberg.org": {
                      "type": "OAuth",
                      "name": "caniko",
                      "token": "oauth-token"
                    }
                  },
                  "aliases": {},
                  "default_ssh": []
                }"#,
            )
            .unwrap();
            std::env::set_var("XDG_DATA_HOME", dir.path());

            let result = codeberg_auth("codeberg.org");
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("has no expires_at for codeberg.org"));
            assert!(err.contains("fj -H codeberg.org auth login"));
            assert!(err.contains("fj -H codeberg.org auth add-token"));
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
            assert!(err.contains("fj -H codeberg.org auth login"));
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
