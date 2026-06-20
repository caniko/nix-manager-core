//! age decryption with identity fallback, and the shared identity-resolution
//! contract used by manager-flake CLIs.
//!
//! [`decrypt_with_identities`] tries identities **sequentially** — one
//! `rage --decrypt` subprocess per identity — and stops at the first success.
//! This avoids a `rage` bug where multiple `AGE-PLUGIN-FIDO2-HMAC` identity
//! stubs passed to a single invocation cause it to hang at "Waiting for
//! age-plugin-fido2-hmac..." without ever prompting for a hardware touch.
//! stderr / stdin stay on the terminal so hardware-key PIN/touch prompts work;
//! only stdout (the plaintext) is captured. A single trailing newline is
//! stripped because agenix payloads are commonly newline-terminated, while
//! downstream secret stores expect the bare value.
//!
//! [`resolve_identities`] implements the configuration surface every manager
//! flow shares (secret-manager's `SECRET_MANAGER_AGE_IDENTITIES`, the DNS
//! flow's `CANIX_DNS_AGE_IDENTITIES`): explicit `--identity` flags win, then a
//! colon-separated environment variable, then the store's master identity
//! stubs under `<root>/age/`.

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ui;

/// A run-scoped age decryption session.
///
/// This currently preserves resolved identity files for sync-run caching.
/// Separate `age-plugin-fido2-hmac` identity stubs cannot be materialized via
/// `age-plugin-fido2-hmac -m`; that flag emits the dataless magic identity, so
/// sync runs must keep passing the original identity paths to `rage`.
#[derive(Debug)]
pub struct DecryptSession {
    identities: Vec<PathBuf>,
}

impl DecryptSession {
    pub fn from_identities(identities: &[PathBuf]) -> Result<Self> {
        if identities.is_empty() {
            bail!("no identities attempted");
        }
        Ok(Self {
            identities: identities.to_vec(),
        })
    }

    pub fn identities(&self) -> &[PathBuf] {
        &self.identities
    }
}

/// Decrypt with a run-scoped session.
pub fn decrypt_with_session(secret: &Path, session: &DecryptSession) -> Result<String> {
    decrypt_with_identities(secret, session.identities())
}

/// Decrypt by trying each identity **sequentially** — one `rage --decrypt`
/// subprocess per identity — and returning the first success.
///
/// See the module-level doc for why this approach avoids a `rage` hang when
/// multiple `AGE-PLUGIN-FIDO2-HMAC` identity stubs are present.
/// Returns the plaintext with one trailing newline stripped.
pub fn decrypt_with_identities(secret: &Path, identities: &[PathBuf]) -> Result<String> {
    if identities.is_empty() {
        bail!("no identities attempted");
    }

    let mut last_err = None;
    for identity in identities {
        match try_decrypt_with_single_identity(secret, identity) {
            Ok(plaintext) => return Ok(plaintext),
            Err(e) => {
                // Propagate Ctrl-C immediately rather than trying more identities.
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|ioe| ioe.kind() == std::io::ErrorKind::Interrupted)
                {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        anyhow!(
            "none of the {} identity(ies) could decrypt {}",
            identities.len(),
            secret.display(),
        )
    }))
}

/// Run `rage --decrypt --identity <identity> <secret>` and return the
/// plaintext. stderr and stdin are inherited so FIDO2 touch prompts work
/// transparently.
fn try_decrypt_with_single_identity(secret: &Path, identity: &PathBuf) -> Result<String> {
    ui::step(format!(
        "decrypting {} with {}",
        secret.display(),
        identity.display(),
    ));

    let out = Command::new("rage")
        .args(rage_decrypt_args(secret, &[identity.clone()]))
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| anyhow!("failed to spawn rage: {e}"))?;
    if !out.status.success() {
        // If the child was killed by a signal (e.g., Ctrl-C during FIDO2 touch
        // prompt), propagate that as an io::Error so the caller can abort the
        // identity fallback loop rather than silently trying the next identity.
        if out.status.code().is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                format!(
                    "rage --decrypt with {} was terminated by signal",
                    identity.display(),
                ),
            ))
            .context("decryption interrupted");
        }
        return Err(anyhow!(
            "rage --decrypt with {} exited with {}",
            identity.display(),
            out.status,
        ));
    }

    let mut value = String::from_utf8(out.stdout)
        .map_err(|_| anyhow!("decrypted payload is not valid UTF-8"))?;
    if value.ends_with('\n') {
        value.pop();
    }
    if value.is_empty() {
        bail!("decrypted payload is empty");
    }
    Ok(value)
}

/// Resolve the identities to decrypt with, in precedence order: explicit
/// `--identity` flags, then the colon-separated `env_var` environment
/// variable, then master identity stubs under `<root>/age/`. Relative paths
/// resolve against `root`; entries that are not files are dropped.
pub fn resolve_identities(
    root: &Path,
    explicit: &[PathBuf],
    env_var: &str,
) -> Result<Vec<PathBuf>> {
    let env_value = std::env::var(env_var).ok();
    resolve_identities_from(root, explicit, env_value.as_deref(), env_var)
}

/// Deterministic variant of [`resolve_identities`] for callers that already
/// read (or synthesize) the environment value.
pub fn resolve_identities_from(
    root: &Path,
    explicit: &[PathBuf],
    env_value: Option<&str>,
    env_var: &str,
) -> Result<Vec<PathBuf>> {
    let env_value = env_value.filter(|value| !value.trim().is_empty());
    let (candidates, source) = if !explicit.is_empty() {
        (explicit.to_vec(), "--identity".to_string())
    } else if let Some(value) = env_value {
        (split_identity_list(value), env_var.to_string())
    } else {
        (
            master_identity_stubs(root),
            format!("master identity stubs in {}", root.join("age").display()),
        )
    };

    let usable: Vec<PathBuf> = candidates
        .iter()
        .map(|p| resolve_against(root, p))
        .filter(|p| p.is_file())
        .collect();
    if usable.is_empty() {
        bail!(
            "no usable age identity files found via {source} — pass --identity, \
             set {env_var} to a colon-separated list of identity paths, \
             or add age/master*identity.pub stubs to the store"
        );
    }
    Ok(usable)
}

/// Master identity stubs in `<root>/age/`, sorted for a stable fallback
/// order. These are typically hardware-key (age plugin) identity files;
/// rage prompts on the controlling terminal.
pub fn master_identity_stubs(root: &Path) -> Vec<PathBuf> {
    let dir = root.join("age");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_master_identity_stub(&name) && path.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Split a colon-separated identity list (the `*_AGE_IDENTITIES` env-var
/// contract), trimming whitespace and dropping empty entries.
pub fn split_identity_list(value: &str) -> Vec<PathBuf> {
    value
        .split(':')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Resolve a path argument against a base when relative.
pub fn resolve_against(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn is_master_identity_stub(name: &str) -> bool {
    (name.starts_with("master-") || name.starts_with("master_")) && name.ends_with(".pub")
}

fn rage_decrypt_args(secret: &Path, identities: &[PathBuf]) -> Vec<OsString> {
    let mut args = Vec::with_capacity(1 + identities.len() * 2 + 1);
    args.push(OsString::from("--decrypt"));
    for identity in identities {
        args.push(OsString::from("--identity"));
        args.push(identity.as_os_str().to_owned());
    }
    args.push(secret.as_os_str().to_owned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::process::Command;

    const ENV_VAR: &str = "TEST_AGE_IDENTITIES";

    struct Fixture {
        _dir: tempfile::TempDir,
        pub_key: PathBuf,
        identity: PathBuf,
    }

    fn setup_fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();

        let key_path = dir.path().join("key.txt");
        let pub_path = dir.path().join("key.pub");

        let out = Command::new("rage-keygen")
            .arg("-o")
            .arg(&key_path)
            .output()
            .expect("rage-keygen must be installed");
        assert!(out.status.success(), "rage-keygen failed: {out:?}");

        let key_data = fs::read_to_string(&key_path).unwrap();
        let pub_line = key_data
            .lines()
            .find_map(|l| l.strip_prefix("# public key: "))
            .expect("public key line in rage-keygen output");
        fs::write(&pub_path, format!("{pub_line}\n")).unwrap();

        Fixture {
            _dir: dir,
            identity: key_path,
            pub_key: pub_path,
        }
    }

    fn encrypt(public_key: &Path, plaintext: &str, dir: &tempfile::TempDir) -> PathBuf {
        let encrypted = dir.path().join("secret.age");
        let recipient = fs::read_to_string(public_key).unwrap().trim().to_string();

        let mut child = Command::new("rage")
            .arg("--encrypt")
            .arg("-r")
            .arg(&recipient)
            .arg("-o")
            .arg(&encrypted)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("rage must be installed");
        {
            let stdin = child.stdin.as_mut().unwrap();
            stdin.write_all(plaintext.as_bytes()).unwrap();
        }
        child.wait().unwrap();
        assert!(encrypted.is_file(), "encrypted file should exist");
        encrypted
    }

    fn touch(path: &Path) {
        fs::write(path, "").unwrap();
    }

    #[test]
    fn decrypts_with_software_identity() {
        let f = setup_fixture();
        let dir = tempfile::tempdir().unwrap();
        let encrypted = encrypt(&f.pub_key, "hello world\n", &dir);

        let result = decrypt_with_identities(&encrypted, &[f.identity]).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn builds_rage_decrypt_args_for_single_identity() {
        let secret = PathBuf::from("secret.age");
        let identity = PathBuf::from("age/master_nitro3c_identity.pub");

        let args = rage_decrypt_args(&secret, &[identity.clone()]);

        assert_eq!(
            args,
            vec![
                OsString::from("--decrypt"),
                OsString::from("--identity"),
                identity.into_os_string(),
                secret.into_os_string(),
            ]
        );
    }

    #[test]
    fn decrypts_with_second_identity_via_fallback() {
        let wrong = setup_fixture();
        let right = setup_fixture();
        let dir = tempfile::tempdir().unwrap();
        let encrypted = encrypt(&right.pub_key, "matched second\n", &dir);

        let result =
            decrypt_with_identities(&encrypted, &[wrong.identity, right.identity]).unwrap();

        assert_eq!(result, "matched second");
    }

    #[test]
    fn strips_trailing_newline() {
        let f = setup_fixture();
        let dir = tempfile::tempdir().unwrap();
        let encrypted = encrypt(&f.pub_key, "trailing newline\n", &dir);

        let result = decrypt_with_identities(&encrypted, &[f.identity]).unwrap();
        assert_eq!(result, "trailing newline");
        assert!(!result.ends_with('\n'));
    }

    #[test]
    fn preserves_content_without_trailing_newline() {
        let f = setup_fixture();
        let dir = tempfile::tempdir().unwrap();
        let encrypted = encrypt(&f.pub_key, "no newline", &dir);

        let result = decrypt_with_identities(&encrypted, &[f.identity]).unwrap();
        assert_eq!(result, "no newline");
    }

    #[test]
    fn errors_on_wrong_identity() {
        let f = setup_fixture();
        let dir = tempfile::tempdir().unwrap();
        let fake_secret = dir.path().join("fake.age");
        fs::write(&fake_secret, "garbage").unwrap();

        let result = decrypt_with_identities(&fake_secret, &[f.identity]);
        assert!(result.is_err());
    }

    #[test]
    fn master_identity_stubs_glob_and_sort() {
        let tmp = tempfile::tempdir().unwrap();
        let age = tmp.path().join("age");
        fs::create_dir_all(&age).unwrap();
        touch(&age.join("master-b-identity.pub"));
        touch(&age.join("master-a-identity.pub"));
        touch(&age.join("master_nitro3c_identity.pub"));
        touch(&age.join("not-a-master.pub"));
        touch(&age.join("master-c-identity.txt"));

        let names: Vec<String> = master_identity_stubs(tmp.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "master-a-identity.pub",
                "master-b-identity.pub",
                "master_nitro3c_identity.pub"
            ]
        );
    }

    #[test]
    fn explicit_flags_win_over_env_and_stubs() {
        let tmp = tempfile::tempdir().unwrap();
        let age = tmp.path().join("age");
        fs::create_dir_all(&age).unwrap();
        touch(&age.join("master-stub-identity.pub"));
        touch(&tmp.path().join("explicit.pub"));
        touch(&tmp.path().join("from-env.pub"));

        let resolved = resolve_identities_from(
            tmp.path(),
            &[PathBuf::from("explicit.pub")],
            Some("from-env.pub"),
            ENV_VAR,
        )
        .unwrap();
        assert_eq!(resolved, vec![tmp.path().join("explicit.pub")]);
    }

    #[test]
    fn env_list_wins_over_stubs_and_resolves_against_root() {
        let tmp = tempfile::tempdir().unwrap();
        let age = tmp.path().join("age");
        fs::create_dir_all(&age).unwrap();
        touch(&age.join("master-stub-identity.pub"));
        touch(&tmp.path().join("first.pub"));
        touch(&tmp.path().join("second.pub"));

        let resolved = resolve_identities_from(
            tmp.path(),
            &[],
            Some("first.pub:second.pub:missing.pub"),
            ENV_VAR,
        )
        .unwrap();
        assert_eq!(
            resolved,
            vec![tmp.path().join("first.pub"), tmp.path().join("second.pub")]
        );
    }

    #[test]
    fn blank_env_falls_back_to_master_stubs() {
        let tmp = tempfile::tempdir().unwrap();
        let age = tmp.path().join("age");
        fs::create_dir_all(&age).unwrap();
        touch(&age.join("master_nitro3c_identity.pub"));

        let resolved = resolve_identities_from(tmp.path(), &[], Some("  "), ENV_VAR).unwrap();
        assert_eq!(resolved, vec![age.join("master_nitro3c_identity.pub")]);
    }

    #[test]
    fn unusable_env_entries_error_with_source_and_remedies() {
        let tmp = tempfile::tempdir().unwrap();
        let err =
            resolve_identities_from(tmp.path(), &[], Some("missing.pub"), ENV_VAR).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(ENV_VAR), "unexpected error: {msg}");
        assert!(msg.contains("--identity"), "unexpected error: {msg}");
    }

    #[test]
    fn no_identities_anywhere_names_the_stub_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_identities_from(tmp.path(), &[], None, ENV_VAR).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("master identity stubs"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn decrypt_session_preserves_resolved_identities() {
        let tmp = tempfile::tempdir().unwrap();
        let fido = tmp.path().join("fido.pub");
        let software = tmp.path().join("software.txt");
        fs::write(&fido, "AGE-PLUGIN-FIDO2-HMAC-test\n").unwrap();
        fs::write(&software, "AGE-SECRET-KEY-1example\n").unwrap();

        let identities = vec![fido, software];
        let session = DecryptSession::from_identities(&identities).unwrap();

        assert_eq!(session.identities(), identities);
    }

    #[test]
    fn decrypt_session_errors_on_empty_identity_list() {
        let err = DecryptSession::from_identities(&[]).unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("no identities attempted"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn split_identity_list_trims_and_drops_empty_entries() {
        assert_eq!(
            split_identity_list(" a.pub : :b.pub:"),
            vec![PathBuf::from("a.pub"), PathBuf::from("b.pub")]
        );
    }

    #[test]
    fn resolve_against_keeps_absolute_paths() {
        assert_eq!(
            resolve_against(Path::new("/store"), Path::new("/abs/key.pub")),
            PathBuf::from("/abs/key.pub")
        );
        assert_eq!(
            resolve_against(Path::new("/store"), Path::new("rel/key.pub")),
            PathBuf::from("/store/rel/key.pub")
        );
    }
}
