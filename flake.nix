{
  description = "Support library and Nix scaffold for manager-flake Rust projects";

  inputs = {
    rs-harbor.url = "git+https://github.com/caniko/rs-harbor.git?ref=trunk&rev=05cc4f162b55fa904b687db1821e2463fa813e50";

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
