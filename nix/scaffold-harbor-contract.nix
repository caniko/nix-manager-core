# Exercise both public scaffold entrypoints against one qualified producer.
{
  self,
  pkgs,
  nixpkgs,
  harbor,
  rust-overlay,
  treefmt-nix,
  git-hooks,
}: let
  mkManagerOutputs = import ./scaffold.nix;
  arguments = {
    inherit self rust-overlay treefmt-nix git-hooks;
    nixpkgs =
      if pkgs.stdenv.hostPlatform.system == "x86_64-darwin"
      then harbor.inputs.nixpkgs-darwin
      else nixpkgs;
    crateName = "nix-manager-core";
  };
  namespace = mkManagerOutputs (arguments // {inherit harbor;});
  legacy = mkManagerOutputs (arguments // {harbor-rs = harbor.legacy.harbor-rs;});
  system = pkgs.stdenv.hostPlatform.system;
  rejected = extras: !(builtins.tryEval (mkManagerOutputs (arguments // extras))).success;
in
  assert namespace.packages.${system}.default.drvPath == legacy.packages.${system}.default.drvPath;
  assert namespace.formatter.${system}.drvPath == legacy.formatter.${system}.drvPath;
  assert rejected {};
  assert rejected {
    inherit harbor;
    harbor-rs = harbor.legacy.harbor-rs;
  };
    pkgs.runCommand "nix-manager-core-scaffold-harbor-contract" {} ''
      touch "$out"
    ''
