//! Authenticated-account OpenPGP registration for Forgejo and GitHub.
//!
//! API contracts: <https://codeberg.org/api/swagger> and
//! <https://docs.github.com/en/rest/users/gpg-keys>.

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::{Command, Stdio};

use super::{codeberg_auth, codeberg_client, require_authenticated_user};

/// Account-level metadata. `public_key` is a base64-encoded public-key packet,
/// not ASCII armor; callers must inspect it before treating a key as identical.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GpgKey {
    pub id: i64,
    pub key_id: String,
    pub public_key: String,
    pub can_sign: bool,
    pub emails: Vec<GpgEmail>,
    /// Forgejo's proof-of-possession flag; GitHub omits this field.
    pub verified: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GpgEmail {
    pub email: String,
    pub verified: bool,
}

#[derive(Debug)]
pub struct AccountGpgKeys {
    pub login: String,
    pub keys: Vec<GpgKey>,
}

/// List every page of the authenticated Forgejo user's GPG keys.
pub fn list_forgejo_keys(host: &str) -> Result<AccountGpgKeys> {
    validate_host(host)?;
    let auth = codeberg_auth(host)?;
    let api = codeberg_client(host, &auth)?;
    list_forgejo_keys_with_api(host, &api)
}

fn list_forgejo_keys_with_api(
    host: &str,
    api: &forgejo_api::sync::Forgejo,
) -> Result<AccountGpgKeys> {
    let login = require_authenticated_user(host, api)?;
    let mut keys = Vec::new();
    for page in 1..=10_000 {
        let (_, batch) = api
            .user_current_list_gpg_keys()
            .page(page)
            .page_size(50)
            .send()
            .with_context(|| {
                format!("listing GPG keys on {host}; the fj token needs read:user permission")
            })?;
        let empty = batch.is_empty();
        for key in batch {
            keys.push(decode_forgejo_key(key)?);
        }
        // A server can cap limit below our requested page size. Only an empty
        // page proves exhaustion, rather than silently overlooking later keys.
        if empty {
            return Ok(AccountGpgKeys { login, keys });
        }
    }
    bail!("GPG-key pagination on {host} exceeded 10000 pages")
}

/// Register public armor on the account discovered during listing. Rechecking
/// the login prevents an auth-store change from publishing to another account.
pub fn add_forgejo_key(host: &str, login: &str, armored_public_key: &str) -> Result<GpgKey> {
    validate_host(host)?;
    validate_public_armor(armored_public_key)?;
    let auth = codeberg_auth(host)?;
    let api = codeberg_client(host, &auth)?;
    ensure!(
        require_authenticated_user(host, &api)? == login,
        "authenticated account changed on {host}; retry publication"
    );
    let key = api
        .user_current_post_gpg_key(forgejo_api::structs::CreateGPGKeyOption {
            armored_public_key: armored_public_key.to_string(),
            armored_signature: None,
        })
        .send()
        .with_context(|| {
            format!("registering GPG key on {host}; the fj token needs write:user permission")
        })?;
    decode_forgejo_key(key)
}

fn decode_forgejo_key(key: forgejo_api::structs::GPGKey) -> Result<GpgKey> {
    serde_json::from_value(serde_json::to_value(key)?)
        .context("Forgejo returned incomplete GPG-key metadata")
}

/// List all GitHub GPG-key pages using the existing gh login.
pub fn list_github_keys() -> Result<AccountGpgKeys> {
    let login = github_login()?;
    let keys = decode_github_pages(&github_api(
        &["user/gpg_keys", "--paginate", "--slurp"],
        None,
    )?)?;
    Ok(AccountGpgKeys { login, keys })
}

pub fn add_github_key(login: &str, armored_public_key: &str) -> Result<GpgKey> {
    validate_public_armor(armored_public_key)?;
    ensure!(
        github_login()? == login,
        "authenticated GitHub account changed; retry publication"
    );
    let payload =
        serde_json::to_vec(&serde_json::json!({"armored_public_key": armored_public_key}))?;
    serde_json::from_slice(&github_api(
        &["user/gpg_keys", "--method", "POST", "--input", "-"],
        Some(&payload),
    )?)
    .context("GitHub returned incomplete GPG-key metadata")
}

fn github_login() -> Result<String> {
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    let user: User = serde_json::from_slice(&github_api(&["user"], None)?)?;
    ensure!(
        !user.login.trim().is_empty(),
        "GitHub returned an empty authenticated login"
    );
    Ok(user.login)
}

fn decode_github_pages(bytes: &[u8]) -> Result<Vec<GpgKey>> {
    let pages: Vec<Vec<GpgKey>> =
        serde_json::from_slice(bytes).context("GitHub returned incomplete GPG-key pages")?;
    Ok(pages.into_iter().flatten().collect())
}

fn github_api(args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut child = Command::new("gh")
        .args(["api", "--hostname", "github.com"])
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run gh api for GPG registration")?;
    if let Some(bytes) = input {
        let result = child
            .stdin
            .take()
            .context("gh stdin was not captured")?
            .write_all(bytes);
        if let Err(err) = result {
            let _ = child.wait();
            return Err(err).context("write public key to gh");
        }
    }
    let output = child.wait_with_output()?;
    ensure!(output.status.success(), "gh api failed: {}\nGPG listing requires read:gpg_key; publication requires write:gpg_key. Refresh gh authentication if needed.", String::from_utf8_lossy(&output.stderr).trim());
    Ok(output.stdout)
}

fn validate_host(host: &str) -> Result<()> {
    let url = url::Url::parse(&format!("https://{host}"))?;
    ensure!(
        url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && !host.chars().any(char::is_whitespace),
        "expected a Forgejo host name, not a URL or path: {host}"
    );
    Ok(())
}

fn validate_public_armor(armor: &str) -> Result<()> {
    ensure!(
        armor
            .trim()
            .starts_with("-----BEGIN PGP PUBLIC KEY BLOCK-----")
            && armor.trim().ends_with("-----END PGP PUBLIC KEY BLOCK-----")
            && !armor.contains("PRIVATE KEY"),
        "expected ASCII-armored OpenPGP public key"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn key(id: i64) -> serde_json::Value {
        json!({
            "id": id, "key_id": "1FA180C8C14B2CAA", "public_key": "public-packet",
            "can_sign": true, "verified": false,
            "created_at": null, "expires_at": null,
            "emails": [{"email": "signer@example.org", "verified": true}]
        })
    }

    #[test]
    fn github_paginated_response_preserves_keys_and_email_verification() {
        let keys = decode_github_pages(&serde_json::to_vec(&json!([[key(1)], [key(2)]])).unwrap())
            .unwrap();
        assert_eq!(keys.iter().map(|key| key.id).collect::<Vec<_>>(), [1, 2]);
        assert!(keys[0].emails[0].verified);
        assert_eq!(keys[0].verified, Some(false));
    }

    #[test]
    fn incomplete_remote_keys_are_rejected() {
        assert!(decode_github_pages(br#"[[{"id": 1, "key_id": "1FA180C8C14B2CAA"}]]"#).is_err());
    }

    #[test]
    fn forgejo_listing_follows_pages_and_resolves_authenticated_account() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (path, body) in [
                (
                    "/api/v1/user",
                    json!({"login": "fixture-user", "avatar_url": null, "html_url": null, "created": null, "last_login": null}),
                ),
                (
                    "/api/v1/user/gpg_keys?page=1&limit=50",
                    json!((1..=50).map(key).collect::<Vec<_>>()),
                ),
                ("/api/v1/user/gpg_keys?page=2&limit=50", json!([key(51)])),
                ("/api/v1/user/gpg_keys?page=3&limit=50", json!([])),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                assert_eq!(request.split_whitespace().next(), Some("GET"));
                assert_eq!(
                    request
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .trim_end_matches('?'),
                    path
                );
                let body = body.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let api = forgejo_api::sync::Forgejo::new(
            forgejo_api::Auth::Token("fixture-token"),
            url::Url::parse(&format!("http://{address}")).unwrap(),
        )
        .unwrap();
        let account = list_forgejo_keys_with_api("fixture", &api).unwrap();
        assert_eq!(account.login, "fixture-user");
        assert_eq!(account.keys.len(), 51);
        assert_eq!(account.keys.last().unwrap().id, 51);
        server.join().unwrap();
    }

    #[test]
    fn publication_rejects_private_key_armor() {
        assert!(validate_public_armor(
            "-----BEGIN PGP PRIVATE KEY BLOCK-----\nprivate\n-----END PGP PRIVATE KEY BLOCK-----"
        )
        .is_err());
    }
}
