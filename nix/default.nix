# nix-manager-core's own flake output assembly — dogfoods the scaffold.
{
  self,
  nixpkgs,
  harbor-rs,
  rust-overlay,
  treefmt-nix,
  git-hooks,
  ...
}: let
  mkManagerOutputs = import ./scaffold.nix;
  outputs = mkManagerOutputs {
    inherit self nixpkgs harbor-rs rust-overlay treefmt-nix git-hooks;
    crateName = "nix-manager-core";
    extraDevShellPackages = pkgs: [pkgs.cargo-deny];
    extraOutputs = {
      lib,
      forAllSystems,
      cargoFor,
      pkgsFor,
      ...
    }: {
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
    lib.mkManagerOutputs = import ./scaffold.nix;
    lib.mkDeclarativeManager = import ./declarative-manager.nix;
  }
