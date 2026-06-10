//! age decryption with identity fallback.
//!
//! [`decrypt_with_identities`] tries each identity in turn with
//! `rage --decrypt --identity <path>`.  stderr / stdin stay on the terminal so
//! hardware-key PIN/touch prompts work; only stdout (the plaintext) is captured.
//! A single trailing newline is stripped — agenix payloads are newline-terminated
//! but most consumers (e.g. Actions secrets) want the bare value.

use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ui;

/// Try each identity in turn with `rage --decrypt`.  The first successful
/// decryption wins.  Returns the plaintext with one trailing newline stripped.
pub fn decrypt_with_identities(secret: &Path, identities: &[PathBuf]) -> Result<String> {
    let mut last_err = None;
    for identity in identities {
        ui::step(format!(
            "decrypting {} with {}",
            secret.display(),
            identity.display()
        ));
        let out = Command::new("rage")
            .arg("--decrypt")
            .arg("--identity")
            .arg(identity)
            .arg(secret)
            .stdin(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| anyhow!("failed to spawn rage: {e}"))?;
        if out.status.success() {
            let mut value = String::from_utf8(out.stdout)
                .map_err(|_| anyhow!("decrypted payload is not valid UTF-8"))?;
            if value.ends_with('\n') {
                value.pop();
            }
            if value.is_empty() {
                bail!("decrypted payload is empty");
            }
            return Ok(value);
        }
        last_err = Some(anyhow!(
            "rage --decrypt --identity {} exited with {}",
            identity.display(),
            out.status
        ));
    }
    Err(last_err.unwrap_or_else(|| anyhow!("no identities attempted")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::process::Command;

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

    #[test]
    fn decrypts_with_software_identity() {
        let f = setup_fixture();
        let dir = tempfile::tempdir().unwrap();
        let encrypted = encrypt(&f.pub_key, "hello world\n", &dir);

        let result = decrypt_with_identities(&encrypted, &[f.identity]).unwrap();
        assert_eq!(result, "hello world");
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
}
