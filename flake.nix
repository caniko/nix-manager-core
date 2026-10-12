{
  description = "Support library and Nix scaffold for manager-flake Rust projects";

  inputs = {
    harbor.url = "git+https://github.com/caniko/harbor.git?ref=feat/harbor-monorepo-components&rev=e8b1996580e4ecd51dd402edf4a16d9df88e3a1b";

    nixpkgs.follows = "harbor/nixpkgs";
    rust-overlay.follows = "harbor/rust-overlay";
    crane.follows = "harbor/crane";

    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";

    git-hooks.url = "github:cachix/git-hooks.nix";
    # git-hooks imports its own package set, including on Intel macOS.
    git-hooks.inputs.nixpkgs.follows = "harbor/nixpkgs-darwin";
  };

  outputs = inputs: import ./nix inputs;
}
