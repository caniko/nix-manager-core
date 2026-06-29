use anyhow::{Context, Result};
use std::process::Command;

/// A target host reachable via SSH.
#[derive(Debug, Clone)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub connect_timeout_secs: u64,
}

impl SshTarget {
    pub fn new(host: &str, port: u16, user: &str) -> Self {
        Self {
            host: host.to_string(),
            port,
            user: user.to_string(),
            connect_timeout_secs: 5,
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.connect_timeout_secs = secs;
        self
    }

    /// Build the SSH args for any subcommand.
    pub fn ssh_args(&self) -> Vec<String> {
        vec![
            "-p".into(),
            self.port.to_string(),
            "-o".into(),
            format!("ConnectTimeout={}", self.connect_timeout_secs),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            format!("{}@{}", self.user, self.host),
        ]
    }

    /// Run a command on the remote host.
    pub fn run(&self, cmd: &str) -> Result<std::process::ExitStatus> {
        let mut args = self.ssh_args();
        args.push(cmd.to_string());
        let status = Command::new("ssh")
            .args(&args)
            .status()
            .with_context(|| format!("ssh to {}@{}:{}", self.user, self.host, self.port))?;
        Ok(status)
    }

    /// Build a `Command` for running via stdin piping or custom stdio.
    pub fn command(&self, cmd: &str) -> Command {
        let mut c = Command::new("ssh");
        c.args(self.ssh_args());
        c.arg(cmd);
        c
    }

    /// Capture stdout from a remote command.
    pub fn capture(&self, cmd: &str) -> Result<String> {
        let mut args = self.ssh_args();
        args.push(cmd.to_string());
        let output = Command::new("ssh")
            .args(&args)
            .output()
            .with_context(|| format!("ssh capture from {}@{}:{}", self.user, self.host, self.port))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "ssh to {}@{} failed (exit: {:?}): {}",
                self.user,
                self.host,
                output.status.code(),
                stderr.trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}
