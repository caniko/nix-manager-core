# Cargo checks reusing dependency artifacts from the package build.
{
  pkgs,
  craneLib,
  commonArgs,
  cargoArtifacts,
  src,
}: {
  clippy = craneLib.cargoClippy (
    commonArgs
    // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-targets -- --deny warnings";
    }
  );

  fmt = craneLib.cargoFmt {inherit src;};

  nextest = craneLib.cargoNextest (
    commonArgs
    // {
      inherit cargoArtifacts;
      nativeBuildInputs = [pkgs.rage];
      partitions = 1;
      partitionType = "count";
      cargoNextestExtraArgs = "--no-tests pass";
    }
  );
}
