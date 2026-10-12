# Reusable flake-output builder for manager-style Rust projects.
#
# Usage from a downstream flake's `nix/default.nix`:
#
#   { self, nixpkgs, harbor, rust-overlay, treefmt-nix, git-hooks, ... }:
#   let
#     inherit (builtins) getFlake;
#     nmc = getFlake "git+https://github.com/caniko/nix-manager-core";
#   in
#     nmc.lib.mkManagerOutputs {
#       inherit self nixpkgs harbor rust-overlay treefmt-nix git-hooks;
#       crateName = "my-manager";
#       srcDir = ../.;
#       extraDevShellPackages = pkgs: [ pkgs.bind ];
#       extraOutputs = { self, lib, forAllSystems, pkgsFor, cargoFor }: {
#         nixosModules.default = import ./module.nix;
#       };
#     }
{
  self,
  nixpkgs,
  harbor ? null,
  # Compatibility for existing scaffold consumers; new callers pass harbor.
  harbor-rs ? null,
  rust-overlay,
  treefmt-nix,
  git-hooks,
  crateName,
  srcDir ? ../.,
  extraOutputs ? {
    self,
    lib,
    forAllSystems,
    pkgsFor,
    cargoFor,
  }: {},
  extraDevShellPackages ? (pkgs: []),
  extraRuntimePackages ? (pkgs: []),
  # Rust edition rustfmt formats against; match the consumer workspace's
  # `workspace.package.edition`.
  rustEdition ? "2021",
  # Optional consumer-owned treefmt module, shared by formatter and hooks.
  treefmtModule ? null,
} @ args:
assert nixpkgs.lib.assertMsg ((harbor != null) != (harbor-rs != null))
"mkManagerOutputs: pass exactly one of harbor or the legacy harbor-rs argument"; let
  inherit (nixpkgs) lib;
  rustLib =
    if harbor != null
    then harbor.lib.rust
    else harbor-rs.lib;
  harborCiFor = system:
    if harbor != null
    then harbor.packages.${system}.rust-harbor-ci
    else harbor-rs.packages.${system}.harbor-ci;

  systems = [
    "x86_64-linux"
    "aarch64-linux"
    "x86_64-darwin"
    "aarch64-darwin"
  ];

  forAllSystems = f: lib.genAttrs systems f;

  pkgsFor = system:
    import (
      if harbor != null && system == "x86_64-darwin"
      then harbor.inputs.nixpkgs-darwin
      else nixpkgs
    ) {
      inherit system;
      overlays = [(import rust-overlay)];
    };

  cargoFor = system:
    import ./package.nix {
      pkgs = pkgsFor system;
      inherit rustLib crateName srcDir extraRuntimePackages;
    };

  treefmtConfig =
    if treefmtModule != null
    then treefmtModule
    else import ./treefmt.nix {inherit rustEdition;};
  preCommitConfig = import ./pre-commit.nix;
  checksConfig = import ./checks.nix;

  base = {
    packages = forAllSystems (
      system: let
        cargo = cargoFor system;
      in {
        default = cargo.package;
        "${crateName}" = cargo.package;
      }
    );

    checks = forAllSystems (
      system: let
        pkgs = pkgsFor system;
        cargo = cargoFor system;
        treefmtEval = treefmt-nix.lib.evalModule pkgs treefmtConfig;
      in
        (checksConfig {
          inherit
            (cargo)
            craneLib
            commonArgs
            cargoArtifacts
            ;
        })
        // {
          formatting = treefmtEval.config.build.check self;
          # Retain the public gate through the shared treefmt policy.
          fmt = self.checks.${system}.formatting;
        }
    );

    devShells = forAllSystems (
      system: let
        pkgs = pkgsFor system;
        cargo = cargoFor system;
        treefmtEval = treefmt-nix.lib.evalModule pkgs treefmtConfig;
        pre-commit-check = git-hooks.lib.${system}.run {
          src = srcDir;
          install.enable = false;
          hooks = preCommitConfig {
            inherit pkgs;
            treefmtWrapper = treefmtEval.config.build.wrapper;
          };
        };
      in {
        default = cargo.craneLib.devShell {
          checks = self.checks.${system};
          packages =
            [(harborCiFor system) treefmtEval.config.build.wrapper]
            ++ (with pkgs;
              [
                cargo-deny
                cargo-nextest
                pre-commit
                rage
                rust-analyzer
              ]
              ++ extraDevShellPackages pkgs
              ++ pre-commit-check.enabledPackages);
          shellHook = pre-commit-check.shellHook;
        };
      }
    );

    formatter = forAllSystems (
      system: (treefmt-nix.lib.evalModule (pkgsFor system) treefmtConfig).config.build.wrapper
    );
  };

  extras = extraOutputs {
    inherit self lib forAllSystems pkgsFor cargoFor;
  };

  mergeSystems = name:
    forAllSystems (
      system:
        (base.${name}.${system} or {}) // (extras.${name}.${system} or {})
    );

  systemAttrs = ["packages" "checks" "devShells" "formatter"];
in
  (builtins.removeAttrs (base // extras) systemAttrs)
  // (builtins.listToAttrs (map (name: {
      inherit name;
      value = mergeSystems name;
    })
    systemAttrs))
