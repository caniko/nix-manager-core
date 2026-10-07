# nix-manager-core

Shared Rust support for Nix manager applications. The specialist library owns
native Nix execution and exact store evidence; consumers supply fleet policy,
credentials, evaluation, publication, and activation.

## Native build frontier

`build::frontier` exposes static input-addressed graphs keyed by derivation and
named output. It parses legacy and Nix v4 derivation JSON, classifies substitutions
using a bounded dry-run, and rejects dynamic or floating outputs.

`Native::plan` accepts already evaluated `Output` goals; it does not evaluate
flakes. `Native::retain` pins sources, derivations, and future output paths before
dispatch. The GC-root directory must be provisioned by the consumer under the
local Nix GC-root tree. Sources and derivations must remain rooted while planning.

`Native::realise` executes one selected goal with local build concurrency capped
at one and remote builders disabled. Restore-only nodes use `max-jobs = 0`, so a
missing substitute fails instead of compiling. `Native::valid` checks store
evidence. Planning and queries have their own short deadlines; realization has
a separately configured worker timeout.

The library also provides repository, age, forge, health, progress, execution,
reconciliation, and source-management support. See the repository README for
module details and the Nix scaffold, which is distributed separately from Cargo.

## Development

From the repository root, use the approved development environment:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo package -p nix-manager-core --allow-dirty
```

The package is being prepared for its first registry publication. Versioned
registry consumption is qualified only after hosted review, CI, and publication
complete; a local package build does not establish publication.
