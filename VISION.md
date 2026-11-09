# nix-manager-core vision

`nix-manager-core` is the shared engine for manager-flake projects: a Rust
support library plus a Nix build scaffold that domain repositories use instead
of each re-implementing terminal output, shell execution, repository discovery,
secret decryption, forge credentials, or exact Nix build dispatch. Its README
names `canix`, `dns-manager`, and `secret-manager` as the data repos that follow
the two-repo pattern with this engine repo; the canix CLI crates depend on it
directly.

## Problem

Manager repositories are data repositories: they carry Nix modules, Pkl data,
and commands that reconcile a fleet. The parts that are not data — bounded
process execution, atomic secret-adjacent file writes, GC-root retention, exact
derivation inspection, health-suite reporting, declarative check/plan/apply —
were being rewritten per repository, and every rewrite re-decided ownership
questions that are easy to get wrong.

## Principles

- **The library supplies evidence; the caller owns policy.** `build::frontier`
  takes already-evaluated derivations and never publishes or activates a
  deployment. Retention, placement, admission, and leases stay caller-owned.
- **Restore-only means restore-only.** A substitutable node may not fall back to
  compilation; losing a substitute fails the goal instead of starting an
  uncoordinated build.
- **Bounded by construction.** Deadlines, cancellation, worker counts, and
  single-job compile dispatch are explicit arguments, not ambient state.
- **Ownership is checked, not assumed.** Callers re-check remote admission after
  prerequisite validation, cancellable workers clean up their process group
  before returning, and GC roots are created directly for immutable inputs.
- **Evidence formats are versioned protocols.** `cache_pin` is a read-only
  payload with an explicit schema version, not a resolver or apply engine.
- **Secret-adjacent writes use one protocol.** `fs::atomic_write_0600` writes,
  fsyncs, chmods, verifies, renames, and re-verifies mode and parent directory.
- **Permissive dependency policy.** `deny.toml` holds the accepted licenses and
  the required Harbor gate runs license and advisory checks.

## Scope

Rust modules: `ui`, `exec`, `exec_bounded`, `fs`, `repo`, `user`, `age`,
`forge` (including `forge::gpg`), `parallel`, `ssh`, `journal`, `sources`,
`reconcile`, `health`, `build` (including `build::frontier`), and `cache_pin`.
The Nix scaffold `nix-manager-core.lib.mkManagerOutputs` returns `packages`,
`checks`, `devShells`, and `formatter` for the four standard systems, with
`extraOutputs` for domain outputs.

## Non-goals

- No deployment activation, publication, or scheduler policy.
- `cache_pin` resolves and applies nothing.
- Real frontier dispatch and performance qualification belong to a consumer's
  integration gate; parser tests do not establish those results.

## Direction

0.3.2 added live remote admission and cancellable exact-output workers; the
head commits add deadline-bound, cancellable execution with raw file logs and
attributed failures. The README records that the `indicatif` 0.18 spinner type
change requires the next minor release.

## Place in the fleet

canix's `cli/crates/canix-foundation`, `canix-commands`, and `canix-ops` depend
on it, and `canix/flake.nix` pins it as an input; the README names `dns-manager`
and `secret-manager` as further data repos.
