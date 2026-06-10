# nix-manager-core's own flake output assembly — dogfoods the scaffold.
{
  self,
  nixpkgs,
  rs-harbor,
  rust-overlay,
  treefmt-nix,
  git-hooks,
  ...
}: let
  mkManagerOutputs = import ./scaffold.nix;
  outputs = mkManagerOutputs {
    inherit self nixpkgs rs-harbor rust-overlay treefmt-nix git-hooks;
    crateName = "nix-manager-core";
  };
in
  outputs
  // {
    lib.mkManagerOutputs = import ./scaffold.nix;
  }
