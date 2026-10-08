//! Exact, dependency-ready remote realization through Nix's builder hook.
use super::*;
use std::collections::BTreeMap;

impl Native {
    /// Realize one static build unit remotely and verify its named output in the
    /// caller's store. The graph and its paths must have been retained with
    /// [`Self::retain`] before releasing the caller's evaluation lease.
    ///
    /// Every immediate input is required locally before dispatch. A second,
    /// substitution-disabled dry-run rejects any unplanned prerequisite build.
    /// Nix owns input closure transfer, remote slot/output locks and output
    /// transfer. Restore-only goals never enter this API. The declared remote
    /// machine must use a trusted `ssh-ng` endpoint, with remote delegation
    /// disabled by its deployment policy.
    pub fn realise_remote(&self, node: &Node, graph: &[Node], machine: &str) -> Result<()> {
        self.realise_remote_if(
            node,
            graph,
            machine,
            || Ok(true),
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        Ok(())
    }

    /// Recheck caller-owned admission after validating and retaining all inputs,
    /// immediately before remote submission. `false` means no remote worker was
    /// started; the caller may place the goal locally. Errors after submission
    /// remain failures and must not be interpreted as a placement refusal.
    pub fn realise_remote_if(
        &self,
        node: &Node,
        graph: &[Node],
        machine: &str,
        admit: impl FnOnce() -> Result<bool>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<bool> {
        ensure!(
            !node.restore_only,
            "restore-only goals cannot compile remotely"
        );
        validate_machine(machine, &self.system)?;
        let definitions: BTreeMap<_, _> = graph.iter().map(|n| (&n.output, n)).collect();
        let recorded = definitions
            .get(&node.output)
            .context("remote goal absent from retained graph")?;
        ensure!(
            recorded.path == node.path
                && recorded.dependencies == node.dependencies
                && !recorded.restore_only,
            "remote goal differs from retained graph"
        );
        let mut inputs = BTreeSet::from([node.output.derivation.clone()]);
        for dependency in &node.dependencies {
            let input = definitions
                .get(dependency)
                .context("remote prerequisite absent from graph")?;
            ensure!(
                self.valid(&input.path)?,
                "remote prerequisite is missing: {}",
                input.path
            );
            inputs.insert(input.path.clone());
        }
        // Reinforce the retained graph at this boundary. A GC cannot remove an
        // input between readiness validation and the Nix hook's closure transfer.
        retain_paths(&self.gc_roots, &inputs)?;
        let mut preflight = self.build_args(true, false);
        preflight.extend(["--option".into(), "substitute".into(), "false".into()]);
        preflight.push(node.output.installable()?);
        let result = self.command(&preflight)?;
        ensure!(
            result.status.success(),
            "remote preflight failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let builds = parse_build_plan(
            &String::from_utf8(result.stderr).context("non-UTF8 remote preflight")?,
        )?;
        ensure_exact_build(&node.output.derivation, &builds)?;
        if self.valid(&node.path)? {
            return Ok(true);
        }
        if !admit()? {
            return Ok(false);
        }
        let mut args = self.build_args(false, false);
        args.extend([
            "--option".into(),
            "builders".into(),
            machine.into(),
            "--option".into(),
            "max-jobs".into(),
            "0".into(),
            "--option".into(),
            "substitute".into(),
            "false".into(),
        ]);
        args.push(node.output.installable()?);
        let result = self.command_cancellable(&args, cancel)?;
        ensure!(
            result.status.success(),
            "remote realization failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        ensure!(
            self.valid(&node.path)?,
            "remote realization returned without requested output {}",
            node.path
        );
        Ok(true)
    }
}

fn ensure_exact_build(derivation: &str, builds: &BTreeSet<String>) -> Result<()> {
    ensure!(
        builds.iter().all(|path| path == derivation),
        "remote realization would build unplanned prerequisites"
    );
    Ok(())
}

fn validate_machine(machine: &str, system: &str) -> Result<()> {
    ensure!(
        !machine.contains(['\n', '\r', ';']),
        "remote realization requires one declared machine"
    );
    let fields: Vec<_> = machine.split_whitespace().collect();
    ensure!(
        fields.len() >= 4
            && fields[0].starts_with("ssh-ng://")
            && fields[1].split(',').any(|s| s == system),
        "remote realization requires a compatible ssh-ng machine"
    );
    let slots: u32 = fields[3].parse().context("invalid remote slot count")?;
    ensure!(slots > 0, "remote machine has no slots");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disappearing_prerequisite_cannot_expand_remote_execution() {
        let root = "/nix/store/00000000000000000000000000000000-check.drv";
        let heavy = "/nix/store/00000000000000000000000000000000-heavy.drv";
        assert!(ensure_exact_build(root, &BTreeSet::from([root.into()])).is_ok());
        assert!(ensure_exact_build(root, &BTreeSet::new()).is_ok());
        assert!(ensure_exact_build(root, &BTreeSet::from([root.into(), heavy.into()])).is_err());
        assert!(ensure_exact_build(root, &BTreeSet::from([heavy.into()])).is_err());
    }

    #[test]
    fn remote_policy_rejects_multiple_or_incompatible_machines() {
        let machine = "ssh-ng://nix-ssh@builder:22 x86_64-linux /key 1 1 kvm -";
        assert!(validate_machine(machine, "x86_64-linux").is_ok());
        for candidate in [
            format!("{machine}; {machine}"),
            format!("{machine}\n{machine}"),
            machine.replace("ssh-ng://", "ssh://"),
            machine.replace(" /key 1 ", " /key 0 "),
        ] {
            assert!(validate_machine(&candidate, "x86_64-linux").is_err());
        }
        assert!(validate_machine(machine, "aarch64-linux").is_err());
    }
}
