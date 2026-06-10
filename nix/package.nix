# crane build of a cargo workspace, using the rs-harbor toolchain.
{
  pkgs,
  rs-harbor,
  crateName,
}: let
  toolchain = rs-harbor.lib.mkToolchain {inherit pkgs;};
  inherit (toolchain) craneLib;

  src = craneLib.cleanCargoSource ../.;

  commonArgs = {
    inherit src;
    strictDeps = true;
    pname = crateName;
    version = "0.1.0";
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  package = craneLib.buildPackage (
    commonArgs
    // {
      inherit cargoArtifacts;
      doCheck = false;
    }
  );
in {
  inherit
    package
    craneLib
    commonArgs
    cargoArtifacts
    src
    toolchain
    ;
}
