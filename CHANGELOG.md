# Changelog

## [Unreleased]

## [0.4.0] - 2026-10-12

### Changed

- Upgrade the Forgejo client to forgejo-api 0.11 and its reqwest 0.13 HTTP
  dependency, retaining the synchronous client and Rustls TLS backend.
- Remove the unused walkdir dependency.

### Migration

- `CodebergAuth::forgejo_auth` now returns forgejo-api 0.11's `Auth`. Callers
  constructing Forgejo clients from this public type must upgrade their own
  forgejo-api dependency to 0.11. The authentication and token lookup contracts
  are unchanged.

## [0.3.2] - 2026-10-08

### Added

- Recheck caller-owned remote admission immediately after prerequisite
  validation, allowing refused goals to return to local placement.
- Add cancellable exact-output workers with owned process-group cleanup before
  returning, so callers can retain GC roots and placement leases through drain.

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
