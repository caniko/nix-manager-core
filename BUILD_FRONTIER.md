# Native build frontier

`build::frontier` supplies exact, output-specific construction evidence to a
deployment scheduler. It takes already evaluated derivations; the caller owns
source capture, phase-local evaluation leases, and source/derivation roots.

The backend supports legacy derivation JSON and Nix's version-4 envelope,
including named multi-output dependencies. It rejects dynamic dependencies and
floating/content-addressed outputs. Crossbow retains its own backend.

`Native::plan` runs a bounded recursive derivation inspection and successful
C-locale Nix dry-run. Substitutable parents are restore-only nodes: schedulers
can prune their build-only dependencies. Restore dispatch disables local and
remote compilation and ambient post-build hooks. Losing a substitute fails the
goal instead of starting an uncoordinated build.

`Native::retain` creates direct GC roots for the immutable source, all evaluated
derivations, and named output paths. Deployment provisions the private directory
inside the actual local store's GC-root tree. The roots persist until explicit
operator cleanup. `Native::valid` checks store evidence; a successful worker exit
without the requested valid output is an error.

Each compile dispatch has at most one local Nix job. The scheduler bounds the
number of dispatches; the Nix daemon's host-resource admission remains in force.
The backend uses explicit Nix and timeout executables and immutable settings.
It never publishes or activates a deployment.

## Validation

```sh
cargo test -p nix-manager-core
cargo clippy -p nix-manager-core --all-targets -- -D warnings
treefmt --ci
```

Parser and argument tests cover both JSON forms, multi-output selection,
substitution classification, incompatible platforms, unsupported dynamic and
floating outputs, and restore-only execution flags. Real Nix frontier dispatch
and resource/performance qualification belong to the scheduler's integration
gate; parser tests do not establish those results.
