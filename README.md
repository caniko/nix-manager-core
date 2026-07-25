# nix-manager-core

<!-- simit:badges:start -->

![CI](https://img.shields.io/badge/CI-drift-2088ff) [![Nix](https://img.shields.io/badge/Nix-managed-5277c3)](flake.nix) [![crates.io](https://img.shields.io/badge/crates.io-ready-f46623)](https://crates.io/crates/nix-manager-core)

<!-- simit:badges:end -->

Support library and Nix scaffold for manager-flake Rust projects.

## Architecture

Manager-flake projects follow a two-repo pattern:

- **Engine repo** (this one) — shared Rust library + Nix build
  scaffold consumed by data repos.
- **Data repos** (e.g. `canix`, `dns-manager`, `secret-manager`) —
  domain-specific Nix modules and data that use the engine.

## Rust modules (`crates/nix-manager-core`)

| Module    | Purpose                                                                  |
| --------- | ------------------------------------------------------------------------ |
| [`ui`]    | Terminal output helpers (headers, steps, spinners, confirm typed)        |
| [`exec`]  | Shell execution (`run`, `capture`, `cap`, `replace`, `run_with_spinner`) |
| [`repo`]  | Repository root discovery (walk up for a marker file)                    |
| [`age`]   | age/rage decryption and shared identity resolution (flags → env → stubs) |
| [`forge`] | Push Actions secrets to Codeberg/Forgejo and GitHub                      |

## Nix scaffold (`nix/scaffold.nix`)

`nix-manager-core.lib.mkManagerOutputs` builds a standard set of flake
outputs for a crane-based Rust project:

```nix
{
  self,
  nixpkgs,
  rs-harbor,
  rust-overlay,
  treefmt-nix,
  git-hooks,
  crateName,
  extraOutputs ? { ... }: {},
}
```

Returns `{ packages, checks, devShells, formatter }` for the standard
systems (`x86_64-linux`, `aarch64-linux`, `x86_64-darwin`,
`aarch64-darwin`). Use `extraOutputs` to merge domain-specific
outputs (e.g. `nixosModules`, `apps`, `lib.*` helpers).

The `extraOutputs` function receives `{ lib, forAllSystems, pkgsFor, cargoFor }`
from the scaffold for use in constructing domain outputs.

### Downstream flake example

```nix
{
  inputs = {
    nix-manager-core.url = "git+https://codeberg.org/caniko/nix-manager-core";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    git-hooks.url = "github:cachix/git-hooks.nix";
  };

  outputs = inputs: inputs.nix-manager-core.lib.mkManagerOutputs {
    inherit (inputs) self nixpkgs rust-overlay treefmt-nix git-hooks;
    rs-harbor = inputs.nix-manager-core.inputs.rs-harbor;
    crateName = "my-manager";
  };
}
```

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- --deny warnings
nix flake check
nix build .#nix-manager-core
```
