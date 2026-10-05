//! Static, output-specific native Nix build graphs for dependency-frontier dispatch.
//!
//! Flake evaluation is a caller-owned, phase-local operation. This backend accepts
//! already evaluated derivations, supports legacy and Nix v4 JSON, and refuses
//! dynamic/floating outputs. A dry-run decides substitution before build-only
//! dependencies are enrolled. Restore goals always disable both local and remote
//! compilation; a disappearing substitute produces an error, never an expanded
//! uncoordinated build.

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output as ProcessOutput};

/// Exact input-addressed output identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Output {
    pub derivation: String,
    pub name: String,
}

impl Output {
    pub fn installable(&self) -> Result<String> {
        store_path(&self.derivation)?;
        ensure!(
            self.derivation.ends_with(".drv"),
            "goal must be a derivation"
        );
        ensure!(
            !self.name.is_empty()
                && self
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_+-".contains(&c)),
            "invalid output name"
        );
        Ok(format!("{}^{}", self.derivation, self.name))
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub output: Output,
    pub path: String,
    pub dependencies: BTreeSet<Output>,
    pub restore_only: bool,
}

/// Immutable execution policy, separate from frontend/cache publication policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Native {
    pub nix: PathBuf,
    pub timeout: PathBuf,
    pub timeout_seconds: u64,
    /// Store queries and each planning command have a shorter bounded budget.
    #[serde(default = "default_query_timeout")]
    pub query_timeout_seconds: u64,
    pub system: String,
    /// Direct GC root directory under the actual local store's gcroots tree.
    pub gc_roots: PathBuf,
    pub substitutes: bool,
}

fn default_query_timeout() -> u64 {
    60
}

impl Native {
    fn command(&self, arguments: &[String]) -> Result<ProcessOutput> {
        self.command_with_timeout(arguments, self.query_timeout_seconds)
    }

    fn command_with_timeout(&self, arguments: &[String], seconds: u64) -> Result<ProcessOutput> {
        ensure!(
            self.nix.is_absolute() && self.timeout.is_absolute(),
            "backend tools must be absolute paths"
        );
        ensure!(
            (1..=86_400).contains(&seconds),
            "backend timeout out of range"
        );
        Command::new(&self.timeout)
            .args(["--signal=TERM", "--kill-after=10s"])
            .arg(seconds.to_string())
            .arg(&self.nix)
            .args(arguments)
            .env("LC_ALL", "C")
            .env("NO_COLOR", "1")
            .output()
            .context("run bounded Nix backend command")
    }

    fn capture(&self, arguments: &[String]) -> Result<Vec<u8>> {
        let result = self.command(arguments)?;
        ensure!(
            result.status.success(),
            "Nix backend failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(result.stdout)
    }

    /// Inspect exact static derivations without evaluating an installable/flake.
    pub fn plan(&self, roots: &[Output]) -> Result<Vec<Node>> {
        ensure!(!roots.is_empty(), "empty native root set");
        let mut args = vec![
            "derivation".into(),
            "show".into(),
            "--no-allow-import-from-derivation".into(),
            "--recursive".into(),
        ];
        for root in roots {
            root.installable()?;
            args.push(root.derivation.clone());
        }
        let raw = self.capture(&args)?;
        let mut args = self.build_args(true, false);
        args.extend(
            roots
                .iter()
                .map(Output::installable)
                .collect::<Result<Vec<_>>>()?,
        );
        let plan = self.command(&args)?;
        ensure!(
            plan.status.success(),
            "Nix build planning failed: {}",
            String::from_utf8_lossy(&plan.stderr)
        );
        let builds =
            parse_build_plan(&String::from_utf8(plan.stderr).context("non-UTF8 build plan")?)?;
        let graph = parse_graph(&raw, &builds, &self.system)?;
        for root in roots {
            ensure!(
                graph.iter().any(|node| &node.output == root),
                "missing root output {root:?}"
            );
        }
        Ok(graph)
    }

    fn build_args(&self, dry_run: bool, restore_only: bool) -> Vec<String> {
        let mut args = vec![
            "build".into(),
            "--no-allow-import-from-derivation".into(),
            "--no-link".into(),
            "--option".into(),
            "post-build-hook".into(),
            "".into(),
            "--option".into(),
            "builders".into(),
            "".into(),
            "--option".into(),
            "substitute".into(),
            self.substitutes.to_string(),
            "--option".into(),
            "max-jobs".into(),
            if restore_only { "0" } else { "1" }.into(),
        ];
        if dry_run {
            args.push("--dry-run".into());
        }
        args
    }

    pub fn valid(&self, path: &str) -> Result<bool> {
        store_path(path)?;
        self.valid_entry(path)
    }

    fn valid_entry(&self, path: &str) -> Result<bool> {
        // A symlink is itself an output, even when its target is absent. Only
        // a missing entry is a cache miss; inspection errors must fail closed.
        match fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| format!("inspect output entry {path}"));
            }
        }
        let result = self.command(&[
            "path-info".into(),
            "--no-allow-import-from-derivation".into(),
            path.into(),
        ])?;
        ensure!(
            result.status.success(),
            "existing output has no valid store evidence: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(true)
    }

    pub fn realise(&self, node: &Node) -> Result<()> {
        let mut args = self.build_args(false, node.restore_only);
        args.push(node.output.installable()?);
        let result = self.command_with_timeout(&args, self.timeout_seconds)?;
        ensure!(
            result.status.success(),
            "Nix realization failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        ensure!(
            self.valid(&node.path)?,
            "Nix returned without the requested output {}",
            node.path
        );
        Ok(())
    }

    /// Root sources, derivations and output paths before any dispatch. These are
    /// direct roots, including outputs that will appear later; no realization is
    /// used to register a root. Roots are retained until explicit operator cleanup.
    pub fn retain(&self, source: &str, graph: &[Node]) -> Result<()> {
        let source = source.split_once('#').map_or(source, |(path, _)| path);
        let mut paths = BTreeSet::from([source.to_owned()]);
        for node in graph {
            paths.insert(node.output.derivation.clone());
            paths.insert(node.path.clone());
        }
        retain_paths(&self.gc_roots, &paths)
    }
}

/// Pin exact store evidence without realizing any derivation. Call inside the
/// caller's evaluation/root phase, before releasing its evaluation lease.
pub fn retain_paths(directory: &Path, paths: &BTreeSet<String>) -> Result<()> {
    ensure!(
        directory.is_absolute() && directory.starts_with("/nix/var/nix/gcroots"),
        "GC roots must be inside the local Nix gcroots tree"
    );
    let metadata = fs::symlink_metadata(directory)
        .context("GC root directory must be provisioned by deployment")?;
    ensure!(metadata.is_dir(), "GC root directory must not be a symlink");
    for path in paths {
        store_path(path)?;
        let name = Path::new(&path)
            .file_name()
            .context("store path has no basename")?;
        let root = directory.join(name);
        match fs::read_link(&root) {
            Ok(previous) => ensure!(previous == Path::new(&path), "GC root collision"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => symlink(path, &root)?,
            Err(error) => return Err(error.into()),
        }
    }
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// Nix's successful C-locale dry-run list. Only standalone, validated store paths
/// in the build section count. A warning mentioning a derivation cannot enroll it.
pub fn parse_build_plan(raw: &str) -> Result<BTreeSet<String>> {
    let mut builds = BTreeSet::new();
    let mut in_builds = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.ends_with("will be built:") {
            in_builds = true;
            continue;
        }
        if line.contains("will be fetched") {
            in_builds = false;
            continue;
        }
        if in_builds && line.starts_with("/nix/store/") {
            store_path(line)?;
            ensure!(
                line.ends_with(".drv"),
                "unexpected item in Nix build plan: {line}"
            );
            builds.insert(line.into());
        }
    }
    Ok(builds)
}

fn store_path(raw: &str) -> Result<String> {
    let path = if raw.starts_with("/nix/store/") {
        raw.to_owned()
    } else {
        format!("/nix/store/{raw}")
    };
    let basename = path
        .strip_prefix("/nix/store/")
        .context("not a local store path")?;
    let (hash, name) = basename.split_once('-').context("invalid store basename")?;
    ensure!(
        hash.len() == 32
            && hash
                .bytes()
                .all(|c| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&c)),
        "invalid store hash"
    );
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+-._?=".contains(&c)),
        "invalid store name"
    );
    Ok(path)
}

/// Parse both Nix's original derivation map and the v4 envelope with basename keys.
/// Definitions retain canonical dependencies even when restoration prunes them.
pub fn parse_graph(raw: &[u8], builds: &BTreeSet<String>, system: &str) -> Result<Vec<Node>> {
    let document: Value = serde_json::from_slice(raw)?;
    if let Some(version) = document.get("version") {
        ensure!(
            version.as_u64() == Some(4),
            "unsupported Nix derivation JSON version"
        );
    }
    let drvs = document
        .get("derivations")
        .unwrap_or(&document)
        .as_object()
        .context("missing derivation map")?;
    let mut graph = Vec::new();
    for (path, drv) in drvs {
        let derivation = store_path(path)?;
        ensure!(derivation.ends_with(".drv"), "non-derivation graph entry");
        let native_system = drv
            .get("system")
            .and_then(Value::as_str)
            .context("derivation missing system")?;
        let outputs = drv
            .get("outputs")
            .and_then(Value::as_object)
            .context("missing derivation outputs")?;
        let mut dependencies = BTreeSet::new();
        let inputs = drv
            .get("inputDrvs")
            .or_else(|| drv.pointer("/inputs/drvs"))
            .and_then(Value::as_object)
            .context("missing derivation inputs")?;
        for (input, selection) in inputs {
            if let Some(dynamic) = selection.get("dynamicOutputs") {
                ensure!(
                    dynamic.as_object().is_some_and(|m| m.is_empty()),
                    "dynamic derivation outputs require a different backend"
                );
            }
            let names = selection
                .as_array()
                .or_else(|| selection.get("outputs").and_then(Value::as_array))
                .context("unknown input output selection")?;
            for name in names {
                let output = Output {
                    derivation: store_path(input)?,
                    name: name.as_str().context("non-string input output")?.into(),
                };
                output.installable()?;
                dependencies.insert(output);
            }
        }
        for (name, output) in outputs {
            let path = output
                .get("path")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .context("floating/content-addressed outputs require a different backend")?;
            let output = Output {
                derivation: derivation.clone(),
                name: name.clone(),
            };
            output.installable()?;
            let restore_only = !builds.contains(&derivation);
            ensure!(
                restore_only || native_system == system || native_system == "builtin",
                "native build for incompatible system {native_system}"
            );
            graph.push(Node {
                output,
                path: store_path(path)?,
                dependencies: dependencies.clone(),
                restore_only,
            });
        }
    }
    let outputs: BTreeSet<_> = graph.iter().map(|node| node.output.clone()).collect();
    for node in &graph {
        ensure!(
            node.dependencies.is_subset(&outputs),
            "incomplete output graph for {:?}",
            node.output
        );
    }
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn path(name: &str) -> String {
        format!("/nix/store/00000000000000000000000000000000-{name}")
    }

    #[test]
    fn v4_graph_preserves_exact_multi_output_dependencies_and_substitution() {
        let raw = json!({"version": 4, "derivations": {
            "00000000000000000000000000000000-parent.drv": {
                "system": "x86_64-linux", "outputs": {"out": {"path": "00000000000000000000000000000000-parent"}},
                "inputs": {"drvs": {"00000000000000000000000000000000-dep.drv": {"outputs": ["dev"], "dynamicOutputs": {}}}}
            },
            "00000000000000000000000000000000-dep.drv": {
                "system": "x86_64-linux", "outputs": {"out": {"path": "00000000000000000000000000000000-dep"}, "dev": {"path": "00000000000000000000000000000000-dep-dev"}},
                "inputs": {"drvs": {}}
            }
        }});
        let graph = parse_graph(
            &serde_json::to_vec(&raw).unwrap(),
            &BTreeSet::from([path("parent.drv")]),
            "x86_64-linux",
        )
        .unwrap();
        let parent = graph
            .iter()
            .find(|n| n.output.derivation == path("parent.drv"))
            .unwrap();
        assert!(!parent.restore_only);
        assert_eq!(
            parent.dependencies,
            BTreeSet::from([Output {
                derivation: path("dep.drv"),
                name: "dev".into()
            }])
        );
        assert!(graph
            .iter()
            .filter(|n| n.output.derivation == path("dep.drv"))
            .all(|n| n.restore_only));
    }

    #[test]
    fn legacy_graph_and_unsupported_dynamic_or_floating_outputs() {
        let mut raw = json!({ path("one.drv"): {"system": "aarch64-linux", "outputs": {"out": {"path": path("one")}}, "inputDrvs": {}} });
        assert!(parse_graph(
            &serde_json::to_vec(&raw).unwrap(),
            &BTreeSet::new(),
            "x86_64-linux"
        )
        .is_ok());
        assert!(parse_graph(
            &serde_json::to_vec(&raw).unwrap(),
            &BTreeSet::from([path("one.drv")]),
            "x86_64-linux"
        )
        .is_err());
        raw[&path("one.drv")]["outputs"]["out"] = json!({});
        assert!(parse_graph(
            &serde_json::to_vec(&raw).unwrap(),
            &BTreeSet::new(),
            "x86_64-linux"
        )
        .is_err());
        raw[&path("one.drv")]["outputs"]["out"] = json!({"path": path("one")});
        raw[&path("one.drv")]["inputDrvs"] =
            json!({path("one.drv"): {"outputs": ["out"], "dynamicOutputs": {"out": {}}}});
        assert!(parse_graph(
            &serde_json::to_vec(&raw).unwrap(),
            &BTreeSet::new(),
            "x86_64-linux"
        )
        .is_err());
    }

    #[test]
    fn build_plan_ignores_warning_paths_and_fetches() {
        let raw = format!(
            "warning: {} is not trusted\nthese 2 derivations will be built:\n  {}\n  {}\nthis path will be fetched:\n  {}\n",
            path("ignored.drv"),
            path("a.drv"),
            path("b.drv"),
            path("fetch.drv")
        );
        assert_eq!(
            parse_build_plan(&raw).unwrap(),
            BTreeSet::from([path("a.drv"), path("b.drv")])
        );
        assert!(
            parse_build_plan("this derivation will be built:\n /nix/store/../../../bad.drv")
                .is_err()
        );
    }

    #[test]
    fn restore_arguments_disable_all_compilation_and_ambient_publication() {
        let native = Native {
            nix: "/tools/nix".into(),
            timeout: "/tools/timeout".into(),
            timeout_seconds: 10,
            query_timeout_seconds: 1,
            system: "x86_64-linux".into(),
            gc_roots: "/nix/var/nix/gcroots/user/train".into(),
            substitutes: true,
        };
        let args = native.build_args(false, true);
        assert!(args.windows(3).any(|a| a == ["--option", "max-jobs", "0"]));
        assert!(args.windows(3).any(|a| a == ["--option", "builders", ""]));
        assert!(args
            .windows(3)
            .any(|a| a == ["--option", "post-build-hook", ""]));
        assert!(!args.iter().any(|a| a == "--dry-run"));
        assert!(Output {
            derivation: path("one.drv"),
            name: "out; touch /tmp/pwn".into()
        }
        .installable()
        .is_err());
    }

    #[test]
    fn dangling_output_requires_authoritative_store_evidence() {
        use std::os::unix::fs::PermissionsExt;
        let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join("sh"))
            .find(|path| path.is_file())
            .expect("test environment must supply a POSIX shell")
            .canonicalize()
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let timeout = temp.path().join("timeout");
        let nix = temp.path().join("nix");
        let queries = temp.path().join("queries");
        let rejected = temp.path().join("rejected");
        fs::write(
            &timeout,
            format!("#!{}\nshift 3\nexec \"$@\"\n", shell.display()),
        )
        .unwrap();
        fs::write(
            &nix,
            format!(
                "#!{}\nprintf '%s\\n' \"$1\" >> '{}'\n[ ! -e '{}' ]\n",
                shell.display(),
                queries.display(),
                rejected.display()
            ),
        )
        .unwrap();
        for path in [&timeout, &nix] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let native = Native {
            nix,
            timeout,
            timeout_seconds: 10,
            query_timeout_seconds: 2,
            system: "x86_64-linux".into(),
            gc_roots: temp.path().join("roots"),
            substitutes: true,
        };
        let output = temp.path().join("output");
        symlink("intentionally-absent", &output).unwrap();
        // Use a fixture entry for filesystem/query behavior; the public method
        // separately enforces that callers supply an exact /nix/store path.
        assert!(native.valid_entry(output.to_str().unwrap()).unwrap());
        fs::write(rejected, "invalid in the store database").unwrap();
        assert!(native.valid_entry(output.to_str().unwrap()).is_err());
        assert_eq!(
            fs::read_to_string(queries).unwrap(),
            "path-info\npath-info\n"
        );
    }

    #[test]
    fn missing_output_is_distinct_from_filesystem_inspection_failure() {
        let temp = tempfile::tempdir().unwrap();
        let native = Native {
            nix: temp.path().join("must-not-run"),
            timeout: temp.path().join("must-not-run-either"),
            timeout_seconds: 10,
            query_timeout_seconds: 2,
            system: "x86_64-linux".into(),
            gc_roots: temp.path().join("roots"),
            substitutes: true,
        };
        assert!(!native
            .valid_entry(temp.path().join("missing").to_str().unwrap())
            .unwrap());
        let looping_parent = temp.path().join("loop");
        symlink("loop", &looping_parent).unwrap();
        let error = native
            .valid_entry(looping_parent.join("output").to_str().unwrap())
            .unwrap_err();
        assert!(
            error.to_string().contains("inspect output entry"),
            "{error:#}"
        );
    }

    #[test]
    fn planning_uses_short_query_budgets_and_realization_uses_worker_budget() {
        use std::os::unix::fs::PermissionsExt;
        let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join("sh"))
            .find(|path| path.is_file())
            .expect("test environment must supply a POSIX shell")
            .canonicalize()
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let timeout = temp.path().join("timeout");
        let nix = temp.path().join("nix");
        let log = temp.path().join("deadlines");
        let raw = temp.path().join("graph.json");
        let drv = path("one.drv");
        fs::write(
            &raw,
            serde_json::to_vec(&json!({ &drv: {
                "system": "x86_64-linux", "outputs": {"out": {"path": path("one")}}, "inputDrvs": {}
            }}))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            &timeout,
            format!(
                "#!{}\nprintf '%s\\n' \"$3\" >> '{}'\nshift 3\nexec \"$@\"\n",
                shell.display(),
                log.display()
            ),
        )
        .unwrap();
        fs::write(&nix, format!("#!{}\nif [ \"$1\" = derivation ]; then cat '{}'; exit; fi\nfor arg do if [ \"$arg\" = --dry-run ]; then printf 'this derivation will be built:\\n  {}\\n' >&2; exit; fi; done\nexit 7\n", shell.display(), raw.display(), drv)).unwrap();
        for path in [&timeout, &nix] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let native = Native {
            nix,
            timeout,
            timeout_seconds: 900,
            query_timeout_seconds: 2,
            system: "x86_64-linux".into(),
            gc_roots: "/nix/var/nix/gcroots/fixture".into(),
            substitutes: true,
        };
        let graph = native
            .plan(&[Output {
                derivation: drv,
                name: "out".into(),
            }])
            .unwrap();
        assert!(native.realise(&graph[0]).is_err());
        assert_eq!(fs::read_to_string(log).unwrap(), "2\n2\n900\n");
    }
}
