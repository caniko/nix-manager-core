# Reusable flake-output builder for manager-style Rust projects.
#
# Usage from a downstream flake's `nix/default.nix`:
#
#   { self, nixpkgs, rs-harbor, rust-overlay, treefmt-nix, git-hooks, ... }:
#   let
#     inherit (builtins) getFlake;
#     nmc = getFlake "git+https://codeberg.org/caniko/nix-manager-core";
#   in
#     nmc.lib.mkManagerOutputs {
#       inherit self nixpkgs rs-harbor rust-overlay treefmt-nix git-hooks;
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
  rs-harbor,
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
} @ args: let
  inherit (nixpkgs) lib;

  systems = [
    "x86_64-linux"
    "aarch64-linux"
    "x86_64-darwin"
    "aarch64-darwin"
  ];

  forAllSystems = f: lib.genAttrs systems f;

  pkgsFor = system:
    import nixpkgs {
      inherit system;
      overlays = [(import rust-overlay)];
    };

  cargoFor = system:
    import ./package.nix {
      pkgs = pkgsFor system;
      inherit rs-harbor crateName srcDir extraRuntimePackages;
    };

  treefmtConfig = import ./treefmt.nix {inherit rustEdition;};
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
            src
            ;
        })
        // {
          formatting = treefmtEval.config.build.check self;
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
          packages = with pkgs;
            [
              cargo-nextest
              pre-commit
              rage
              rust-analyzer
            ]
            ++ extraDevShellPackages pkgs
            ++ pre-commit-check.enabledPackages;
          shellHook = pre-commit-check.shellHook;
        };
      }
    );

    apps = forAllSystems (
      system: let
        pkgs = pkgsFor system;
        atticAdapter = rs-harbor.lib.mkAdapter {
          attic = {
            endpoint = "https://attic.candee.baby";
            cache = "canix";
          };
        };
      in {
        push-flake-inputs = rs-harbor.lib.mkAtticPush {
          inherit pkgs;
          adapter = atticAdapter;
          flake = ".";
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

  systemAttrs = ["packages" "checks" "devShells" "apps" "formatter"];
in
  (builtins.removeAttrs (base // extras) systemAttrs)
  // (builtins.listToAttrs (map (name: {
      inherit name;
      value = mergeSystems name;
    })
    systemAttrs))
