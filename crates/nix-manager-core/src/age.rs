//! age decryption with identity fallback, and the shared identity-resolution
//! contract used by manager-flake CLIs.
//!
//! [`decrypt_with_identities`] passes every candidate identity to one
//! `rage --decrypt` invocation. stderr / stdin stay on the terminal so
//! hardware-key PIN/touch prompts work; only stdout (the plaintext) is
//! captured. A single trailing newline is stripped because agenix payloads are
//! commonly newline-terminated, while downstream secret stores expect the bare
//! value.
//!
//! [`resolve_identities`] implements the configuration surface every manager
//! flow shares (secret-manager's `SECRET_MANAGER_AGE_IDENTITIES`, the DNS
//! flow's `CANIX_DNS_AGE_IDENTITIES`): explicit `--identity` flags win, then a
//! colon-separated environment variable, then the store's master identity
//! stubs under `<root>/age/`.

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ui;

const FIDO2_HMAC_IDENTITY_PREFIX: &str = "AGE-PLUGIN-FIDO2-HMAC-";
static SESSION_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A run-scoped age decryption session.
///
/// FIDO2 hmac identities are materialized once into private runtime files and
/// removed when the session is dropped. Plain software identities are reused
/// directly.
#[derive(Debug)]
pub struct DecryptSession {
    identities: Vec<PathBuf>,
    materialized: Vec<PathBuf>,
}

impl DecryptSession {
    pub fn from_identities(identities: &[PathBuf]) -> Result<Self> {
        Self::from_identities_with_optional_runtime_dir(identities, None, materialize_fido2_hmac)
    }

    #[cfg(test)]
    fn from_identities_with(
        identities: &[PathBuf],
        runtime_dir: PathBuf,
        materialize: impl Fn(&Path) -> Result<Vec<u8>>,
    ) -> Result<Self> {
        Self::from_identities_with_optional_runtime_dir(identities, Some(runtime_dir), materialize)
    }

    fn from_identities_with_optional_runtime_dir(
        identities: &[PathBuf],
        mut runtime_dir: Option<PathBuf>,
        materialize: impl Fn(&Path) -> Result<Vec<u8>>,
    ) -> Result<Self> {
        if identities.is_empty() {
            bail!("no identities attempted");
        }

        let mut session_identities = Vec::with_capacity(identities.len());
        let mut materialized = Vec::new();
        let mut prepared_runtime_dir = false;

        for identity in identities {
            if is_fido2_hmac_identity_stub(identity)? {
                let runtime_dir = match &runtime_dir {
                    Some(path) => path,
                    None => runtime_dir.insert(runtime_secret_manager_dir()?),
                };
                if !prepared_runtime_dir {
                    prepare_runtime_dir(&runtime_dir)?;
                    prepared_runtime_dir = true;
                }
                ui::step(format!(
                    "materializing FIDO2 age identity from {} for this sync process; \
                     the temporary identity is mode 0600 and is removed after the run",
                    identity.display()
                ));
                let bytes = materialize(identity)?;
                if bytes.is_empty() {
                    bail!(
                        "age-plugin-fido2-hmac -m {} produced an empty identity",
                        identity.display()
                    );
                }
                let path = write_materialized_identity(&runtime_dir, &bytes)?;
                session_identities.push(path.clone());
                materialized.push(path);
            } else {
                session_identities.push(identity.clone());
            }
        }

        Ok(Self {
            identities: session_identities,
            materialized,
        })
    }

    pub fn identities(&self) -> &[PathBuf] {
        &self.identities
    }
}

impl Drop for DecryptSession {
    fn drop(&mut self) {
        for path in &self.materialized {
            let _ = fs::remove_file(path);
        }
    }
}

/// Decrypt with a run-scoped session.
pub fn decrypt_with_session(secret: &Path, session: &DecryptSession) -> Result<String> {
    decrypt_with_identities(secret, session.identities())
}

/// Decrypt with all identities passed to one `rage --decrypt` invocation.
/// Returns the plaintext with one trailing newline stripped.
pub fn decrypt_with_identities(secret: &Path, identities: &[PathBuf]) -> Result<String> {
    if identities.is_empty() {
        bail!("no identities attempted");
    }

    ui::step(format!(
        "decrypting {} with {}",
        secret.display(),
        describe_identities(identities)
    ));
    let out = Command::new("rage")
        .args(rage_decrypt_args(secret, identities))
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| anyhow!("failed to spawn rage: {e}"))?;
    if !out.status.success() {
        bail!(
            "rage --decrypt with identities {} exited with {}. Check that the matching hardware key is present and touch it when prompted.",
            describe_identities(identities),
            out.status
        );
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

fn is_fido2_hmac_identity_stub(path: &Path) -> Result<bool> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading age identity {}", path.display()))?;
    Ok(raw
        .lines()
        .map(str::trim)
        .any(|line| line.starts_with(FIDO2_HMAC_IDENTITY_PREFIX)))
}

fn runtime_secret_manager_dir() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        anyhow!(
            "XDG_RUNTIME_DIR is required to materialize FIDO2 age identities for one-touch sync; \
             refusing to write the temporary identity to a persistent directory"
        )
    })?;
    if runtime.is_empty() {
        bail!(
            "XDG_RUNTIME_DIR is empty; refusing to materialize FIDO2 age identities outside a private runtime directory"
        );
    }
    Ok(PathBuf::from(runtime).join("secret-manager"))
}

fn prepare_runtime_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("creating private runtime directory {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("setting mode 0700 on {}", path.display()))?;
    let mode = fs::metadata(path)
        .with_context(|| format!("checking permissions on {}", path.display()))?
        .permissions()
        .mode()
        & 0o777;
    if mode != 0o700 {
        bail!(
            "{} has mode {:03o}, expected 700 for materialized FIDO2 identities",
            path.display(),
            mode
        );
    }
    Ok(())
}

fn materialize_fido2_hmac(identity: &Path) -> Result<Vec<u8>> {
    let out = Command::new("age-plugin-fido2-hmac")
        .arg("-m")
        .arg(identity)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| anyhow!("failed to spawn age-plugin-fido2-hmac: {e}"))?;
    if !out.status.success() {
        bail!(
            "age-plugin-fido2-hmac -m {} exited with {}. \
             This command materializes a temporary age identity for the sync process; \
             touch the matching hardware key when prompted.",
            identity.display(),
            out.status
        );
    }
    Ok(out.stdout)
}

fn write_materialized_identity(runtime_dir: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let id = SESSION_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = runtime_dir.join(format!(
        "materialized-age-identity-{}-{id}",
        std::process::id()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| {
            format!(
                "creating temporary materialized identity {}",
                path.display()
            )
        })?;
    file.write_all(bytes)
        .with_context(|| format!("writing temporary materialized identity {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("fsync temporary materialized identity {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("setting mode 0600 on {}", path.display()))?;
    let mode = fs::metadata(&path)
        .with_context(|| format!("checking permissions on {}", path.display()))?
        .permissions()
        .mode()
        & 0o777;
    if mode != 0o600 {
        bail!(
            "{} has mode {:03o}, expected 600 for materialized FIDO2 identity",
            path.display(),
            mode
        );
    }
    Ok(path)
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

fn describe_identities(identities: &[PathBuf]) -> String {
    identities
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::process::Command;
    use std::sync::Mutex;

    const ENV_VAR: &str = "TEST_AGE_IDENTITIES";
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
    fn builds_single_rage_command_with_multiple_identities() {
        let secret = PathBuf::from("secret.age");
        let first = PathBuf::from("age/master-a-identity.pub");
        let second = PathBuf::from("age/master_b_identity.pub");

        let args = rage_decrypt_args(&secret, &[first.clone(), second.clone()]);

        assert_eq!(
            args,
            vec![
                OsString::from("--decrypt"),
                OsString::from("--identity"),
                first.into_os_string(),
                OsString::from("--identity"),
                second.into_os_string(),
                secret.into_os_string(),
            ]
        );
    }

    #[test]
    fn decrypts_with_second_identity_in_single_rage_invocation() {
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
    fn fido2_hmac_identity_stub_detection_uses_identity_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let fido = tmp.path().join("fido.pub");
        fs::write(
            &fido,
            "# created: 2026-06-13T00:00:00Z\n# public key: age1example\nAGE-PLUGIN-FIDO2-HMAC-test\n",
        )
        .unwrap();
        let software = tmp.path().join("software.txt");
        fs::write(&software, "AGE-SECRET-KEY-1example\n").unwrap();

        assert!(is_fido2_hmac_identity_stub(&fido).unwrap());
        assert!(!is_fido2_hmac_identity_stub(&software).unwrap());
    }

    #[test]
    fn decrypt_session_materializes_fido2_identity_mode_0600_and_removes_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime").join("secret-manager");
        let fido = tmp.path().join("fido.pub");
        fs::write(&fido, "AGE-PLUGIN-FIDO2-HMAC-test\n").unwrap();

        let materialized_path = {
            let session =
                DecryptSession::from_identities_with(&[fido], runtime.clone(), |_identity| {
                    Ok(b"AGE-SECRET-KEY-1materialized\n".to_vec())
                })
                .unwrap();

            assert_eq!(session.identities().len(), 1);
            let path = session.identities()[0].clone();
            assert!(path.starts_with(&runtime));
            assert_eq!(
                fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "AGE-SECRET-KEY-1materialized\n"
            );
            path
        };

        assert!(!materialized_path.exists());
    }

    #[test]
    fn decrypt_session_leaves_non_fido_identity_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime").join("secret-manager");
        let software = tmp.path().join("software.txt");
        fs::write(&software, "AGE-SECRET-KEY-1example\n").unwrap();

        let session = DecryptSession::from_identities_with(&[software.clone()], runtime, |_| {
            panic!("non-FIDO identity should not be materialized")
        })
        .unwrap();

        assert_eq!(session.identities(), &[software]);
    }

    #[test]
    fn decrypt_session_does_not_require_xdg_runtime_dir_for_non_fido_identity() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let software = tmp.path().join("software.txt");
        fs::write(&software, "AGE-SECRET-KEY-1example\n").unwrap();

        let old_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::remove_var("XDG_RUNTIME_DIR");
        let session = DecryptSession::from_identities(std::slice::from_ref(&software)).unwrap();
        match old_runtime {
            Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }

        assert_eq!(session.identities(), &[software]);
    }

    #[test]
    fn decrypt_session_requires_xdg_runtime_dir_for_fido2_identity() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let fido = tmp.path().join("fido.pub");
        fs::write(&fido, "AGE-PLUGIN-FIDO2-HMAC-test\n").unwrap();

        let old_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::remove_var("XDG_RUNTIME_DIR");
        let err = DecryptSession::from_identities(&[fido]).unwrap_err();
        match old_runtime {
            Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }

        let msg = err.to_string();
        assert!(msg.contains("XDG_RUNTIME_DIR"), "unexpected error: {msg}");
        assert!(
            msg.contains("refusing to write the temporary identity to a persistent directory"),
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
