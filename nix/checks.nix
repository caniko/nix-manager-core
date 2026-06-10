# Cargo checks reusing dependency artifacts from the package build.
{
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
      partitions = 1;
      partitionType = "count";
    }
  );
}
