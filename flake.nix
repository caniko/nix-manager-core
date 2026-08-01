{
  description = "Support library and Nix scaffold for manager-flake Rust projects";

  inputs = {
    rs-harbor.url = "github:caniko/rs-harbor/e2778ff3beca1bd4c1f5183313251d1fb5b46dd6";

    nixpkgs.follows = "rs-harbor/nixpkgs";
    rust-overlay.follows = "rs-harbor/rust-overlay";
    crane.follows = "rs-harbor/crane";

    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";

    git-hooks.url = "github:cachix/git-hooks.nix";
    git-hooks.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = inputs: import ./nix inputs;
}
