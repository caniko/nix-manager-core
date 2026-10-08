# Changelog

## [Unreleased]

## [0.3.1] - 2026-10-08

### Added

- Add exact dependency-ready remote realization through a declared ssh-ng
  machine. Retain prerequisite paths and reject unexpected prerequisite builds
  before dispatch, keep restore-only execution separate, and verify returned
  named outputs in the caller's store. Nix retains remote slot and output locks.

## [0.3.0] - 2026-10-06

- Expose static named-output native Nix frontiers with substitution classification,
  restore-only realization, and direct GC-root retention.
- Separate short planning/store-query deadlines from realization worker deadlines.
- Prepare the library package metadata, documentation, license, and generated CI
  publication path for its first crates.io release.

Deployment integration and performance qualification remain consumer-owned gates.

[Unreleased]: https://github.com/caniko/nix-manager-core/compare/0.3.0...HEAD
