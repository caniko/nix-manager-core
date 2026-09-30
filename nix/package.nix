# crane build of a cargo workspace, using the harbor-rs toolchain.
{
  pkgs,
  harbor-rs,
  crateName,
  srcDir ? ../.,
  extraRuntimePackages ? pkgs: [],
}: let
  toolchain = harbor-rs.lib.mkToolchain {
    inherit pkgs;
    toolchainProfile = "nightly";
  };
  inherit (toolchain) craneLib;

  src = pkgs.lib.fileset.toSource {
    root = srcDir;
    fileset = pkgs.lib.fileset.unions [
      (craneLib.fileset.commonCargoSources srcDir)
      (pkgs.lib.fileset.maybeMissing (srcDir + "/tests/fixtures"))
      (pkgs.lib.fileset.maybeMissing (srcDir + "/crates/${crateName}/tests/fixtures"))
    ];
  };
  runtimePackages = extraRuntimePackages pkgs;

  commonArgs = {
    inherit src;
    strictDeps = true;
    pname = crateName;
    version = "0.1.0";
    nativeBuildInputs = [pkgs.git pkgs.rage];
    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
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
