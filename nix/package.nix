# crane build of a cargo workspace, using the rs-harbor toolchain.
{
  pkgs,
  rs-harbor,
  crateName,
  srcDir ? ../.,
  extraRuntimePackages ? pkgs: [],
}: let
  toolchain = rs-harbor.lib.mkToolchain {inherit pkgs; toolchainProfile = "nightly";};
  inherit (toolchain) craneLib;

  src = craneLib.cleanCargoSource srcDir;
  runtimePackages = extraRuntimePackages pkgs;

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
      nativeBuildInputs = pkgs.lib.optionals (runtimePackages != []) [
        pkgs.makeWrapper
      ];
      postInstall = pkgs.lib.optionalString (runtimePackages != []) ''
        wrapProgram "$out/bin/${crateName}" \
          --prefix PATH : ${pkgs.lib.makeBinPath runtimePackages}
      '';
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
