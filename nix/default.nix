# nix-manager-core's own flake output assembly — dogfoods the scaffold.
{
  self,
  nixpkgs,
  nixpkgs-darwin,
  harbor-rs,
  rust-overlay,
  treefmt-nix,
  git-hooks,
  ...
}: let
  mkManagerOutputs = args: import ./scaffold.nix ({inherit nixpkgs-darwin;} // args);
  # Keep shell evaluation independent of the checks that exercise it.
  scaffoldOnly = mkManagerOutputs {
    self = self // scaffoldOnly;
    inherit nixpkgs harbor-rs rust-overlay treefmt-nix git-hooks;
    crateName = "nix-manager-core";
  };
  outputs = mkManagerOutputs {
    inherit self nixpkgs harbor-rs rust-overlay treefmt-nix git-hooks;
    crateName = "nix-manager-core";
    extraDevShellPackages = pkgs: [pkgs.cargo-deny pkgs.cargo-audit];
    extraOutputs = {
      lib,
      forAllSystems,
      cargoFor,
      pkgsFor,
      ...
    }: {
      devShells = forAllSystems (system: let
        pkgs = pkgsFor system;
        compatibilityShell = version:
          pkgs.mkShell {
            packages = [pkgs.rust-bin.stable.${version}.default pkgs.git pkgs.rage];
            RUSTFLAGS = "";
            CARGO_ENCODED_RUSTFLAGS = "";
          };
      in {
        msrv = compatibilityShell "1.88.0";
        docs = compatibilityShell "1.96.1";
      });
      checks = forAllSystems (system: let
        pkgs = pkgsFor system;
        cargo = cargoFor system;
        testArgs =
          cargo.commonArgs
          // {
            inherit (cargo) cargoArtifacts;
            src = lib.cleanSourceWith {
              src = ../.;
              filter = path: type:
                cargo.craneLib.filterCargoSources path type
                || builtins.elem path (map
                  (name: "${toString ../.}/crates/nix-manager-core/tests/fixtures/cache-pin/${name}.json")
                  ["cache-provenance" "incompatible-downgrade" "aggregate-failure"]);
            };
          };
      in {
        scaffold-platform-shells = assert builtins.deepSeq (builtins.attrValues (forAllSystems (
          target: scaffoldOnly.devShells.${target}.default.drvPath
        )))
        true;
          pkgs.runCommand "scaffold-platform-shells" {} "touch $out";
        clippy = cargo.craneLib.cargoClippy (testArgs
          // {
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });
        nextest = cargo.craneLib.cargoNextest (testArgs
          // {
            nativeBuildInputs = [pkgs.git pkgs.rage];
            partitions = 1;
            partitionType = "count";
            cargoNextestExtraArgs = "--no-tests pass";
          });
      });
    };
  };
in
  outputs
  // {
    lib.mkManagerOutputs = mkManagerOutputs;
    lib.mkDeclarativeManager = import ./declarative-manager.nix;
  }
