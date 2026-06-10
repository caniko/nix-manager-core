{rustEdition ? "2021"}: {pkgs, ...}: {
  projectRootFile = "flake.nix";

  programs.rustfmt = {
    enable = true;
    edition = rustEdition;
    package = pkgs.rust-bin.nightly.latest.default.override {
      extensions = ["rustfmt"];
    };
  };

  programs.alejandra.enable = true;

  programs.taplo.enable = true;

  programs.prettier = {
    enable = true;
    package = pkgs.prettier;
    includes = [
      "*.md"
      "*.markdown"
    ];
  };
}
