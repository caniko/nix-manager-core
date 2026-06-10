//! Shell execution helpers.
//!
//! Three entry points, picked by your error-handling needs:
//! - [`run`] — run a child inheriting our stdio; error on non-zero exit.
//! - [`capture`] — capture stdout (stderr inherited); error on non-zero exit.
//! - [`cap`] / [`cap_with_env`] — capture exit code, stdout, AND stderr without
//!   ever erroring at the Rust level. Useful for probes where non-zero is just
//!   data (e.g. health checks).
//!
//! Plus a few small utilities ([`is_local_host`], [`local_hostname`],
//! [`tail_lines`]) that span multiple unrelated callers.

use anyhow::{anyhow, Result};
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::ui;

/// Like [`run`], but with the child's working directory set to `dir`.
pub fn run_in<I, S>(dir: &Path, program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let pretty: Vec<String> = std::iter::once(program.to_string())
        .chain(args.iter().map(|a| a.to_string_lossy().into_owned()))
        .collect();
    eprintln!("$ ({}) {}", dir.display(), pretty.join(" "));

    let status = Command::new(program)
        .args(&args)
        .current_dir(dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| anyhow!("failed to spawn {program} in {}: {e}", dir.display()))?;

    if !status.success() {
        return Err(anyhow!(
            "{program} exited with status {} in {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".into()),
            dir.display()
        ));
    }
    Ok(())
}

/// Like [`cap`], but with the child's working directory set to `dir`.
pub fn cap_in<I, S>(dir: &Path, program: &str, args: I) -> Captured
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    match Command::new(program).args(args).current_dir(dir).output() {
        Ok(out) => Captured {
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) => Captured {
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("spawn failed in {}: {e}", dir.display()),
        },
    }
}

pub fn run<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let pretty: Vec<String> = std::iter::once(program.to_string())
        .chain(args.iter().map(|a| a.to_string_lossy().into_owned()))
        .collect();
    eprintln!("$ {}", pretty.join(" "));

    let status = Command::new(program)
        .args(&args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| {
            let path = std::env::var("PATH").unwrap_or_else(|_| "<unset>".into());
            anyhow!("failed to spawn {program}: {e}\n  PATH = {path}")
        })?;

    if !status.success() {
        return Err(anyhow!(
            "{} exited with status {}",
            program,
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".into())
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub fn replace<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    use std::os::unix::process::CommandExt;

    let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let pretty: Vec<String> = std::iter::once(program.to_string())
        .chain(args.iter().map(|a| a.to_string_lossy().into_owned()))
        .collect();
    eprintln!("$ {}", pretty.join(" "));

    let err = Command::new(program).args(&args).exec();
    let path = std::env::var("PATH").unwrap_or_else(|_| "<unset>".into());
    Err(anyhow!("failed to exec {program}: {err}\n  PATH = {path}"))
}

#[cfg(not(unix))]
pub fn replace<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run(program, args)
}

pub fn capture<I, S>(program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| anyhow!("failed to spawn {}: {e}", program))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{} exited with status {}",
            program,
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".into())
        ));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Captured exit code + stdout + stderr from an infallible child invocation.
#[derive(Debug, Clone)]
pub struct Captured {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    pub fn ok(&self) -> bool {
        self.exit_code == 0
    }
}

/// Run `program` with `args`, capturing exit code, stdout, and stderr.
/// Never errors at the Rust level — a spawn failure surfaces as exit_code=-1
/// with the error text on stderr, so callers can encode it as data themselves.
pub fn cap<I, S>(program: &str, args: I) -> Captured
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    cap_with_env(program, args, std::iter::empty::<(&str, &str)>())
}

/// Like [`cap`] but also sets environment variables for the child.
pub fn cap_with_env<I, S, K, V>(
    program: &str,
    args: I,
    envs: impl IntoIterator<Item = (K, V)>,
) -> Captured
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    match cmd.output() {
        Ok(out) => Captured {
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) => Captured {
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("spawn failed: {e}"),
        },
    }
}

/// Spawn `program` with `args`, hiding its stdout/stderr behind a spinner.
/// On success: spinner clears and a green `✔` line replaces it.
/// On failure: spinner clears, captured stderr is dumped, error returned.
pub fn run_with_spinner<I, S>(
    program: &str,
    args: I,
    spinner_msg: impl Into<String>,
    success_msg: impl Into<String>,
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let pb = ui::spinner(spinner_msg);
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| {
            pb.finish_and_clear();
            anyhow!("failed to spawn {program}: {e}")
        })?;

    if !output.status.success() {
        ui::fail_spinner(pb, format!("{program} failed"));
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        return Err(anyhow!(
            "{program} exited with status {}",
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".into())
        ));
    }

    ui::finish_spinner(pb, success_msg);
    Ok(())
}

/// Last `n` lines of a multi-line string, in original order. Empty input
/// yields an empty vec; fewer than `n` lines yields all of them.
pub fn tail_lines(s: &str, n: usize) -> Vec<&str> {
    let v: Vec<&str> = s.lines().collect();
    let start = v.len().saturating_sub(n);
    v[start..].to_vec()
}

/// `nix build --no-link --print-out-paths <attr>` and return the first
/// produced store path.
pub fn nix_build_path(attr: &str) -> Result<String> {
    let raw = capture("nix", ["build", "--no-link", "--print-out-paths", attr])?;
    raw.lines()
        .next()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("nix build produced no output path"))
}

/// Check whether `sudo` can authenticate non-interactively for the current
/// user. This intentionally uses `sudo -n -v`: callers that cannot allocate a
/// tty must fail before constructing a command whose stdout might be empty.
pub fn sudo_noninteractive_ok() -> Captured {
    cap("sudo", ["-n", "-v"])
}

/// `hostname` of the current machine. Errors with a clear diagnostic when the
/// syscall fails or when the bytes don't decode as UTF-8. Callers that prefer
/// an empty string on failure can chain `.unwrap_or_default()`.
pub fn local_hostname() -> Result<String> {
    hostname::get()
        .map_err(|e| anyhow!("hostname: {e}"))?
        .into_string()
        .map_err(|os| anyhow!("hostname is not valid UTF-8: {os:?}"))
}

/// True when `host` refers to the machine this process runs on. Treats
/// `localhost` / `127.0.0.1` / `::1` as local without a hostname syscall.
pub fn is_local_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
        || local_hostname().map(|h| h == host).unwrap_or(false)
}
