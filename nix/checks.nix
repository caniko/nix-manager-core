# Cargo checks reusing dependency artifacts from the package build.
{
  craneLib,
  commonArgs,
  cargoArtifacts,
}: {
  clippy = craneLib.cargoClippy (
    commonArgs
    // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-targets -- --deny warnings";
    }
  );

  nextest = craneLib.cargoNextest (
    commonArgs
    // {
      inherit cargoArtifacts;
      partitions = 1;
      partitionType = "count";
      cargoNextestExtraArgs = "--no-tests pass";
    }
  );
}
