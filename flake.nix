{
  description = "Support library and Nix scaffold for manager-flake Rust projects";

  inputs = {
    harbor-rs.url = "git+https://github.com/caniko/harbor-rs.git?ref=trunk&rev=fac8049316846e0ef1c1e6acd92aed7a337b333a";

    nixpkgs.follows = "harbor-rs/nixpkgs";
    # The maintained 26.05 branch retains Intel macOS support.
    nixpkgs-darwin.url = "github:NixOS/nixpkgs/e16b7a8dc0d467861b053a85f437d7dede44963c";
    rust-overlay.follows = "harbor-rs/rust-overlay";
    crane.follows = "harbor-rs/crane";

    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";

    git-hooks.url = "github:cachix/git-hooks.nix";
    git-hooks.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = inputs: import ./nix inputs;
}
