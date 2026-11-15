# AGENTS.md

`heavy` is a Rust crate: a library (`src/lib.rs`, the pool of `flock` slots) and a command line
(`src/main.rs`) that runs a command while holding a slot.

- Toolchain and tools come from `mise.toml`: `mise exec -- cargo test`, `cargo fmt`,
  `cargo clippy --all-targets -- -D warnings`. All three must pass before a push.
- The tests run the real binary against real processes (`tests/cli.rs`): queueing, `kill -9`,
  nesting, exit codes. A change to the behaviour starts with a test there.
  Their commands run through `sh -c`, on Windows the `sh` of Git for Windows, and CI runs them on
  Linux, macOS and Windows. Code under `#[cfg(windows)]` can be linted from another system with
  `cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`.
- Releases are made by merging the release pull request that release-please opens from the
  Conventional Commits on `main`: it bumps the version and `CHANGELOG.md`, and the tag it pushes
  (with `RELEASE_PLEASE_TOKEN`, so that the tag starts a workflow) runs `release.yml`. `dist`
  (cargo-dist) generates that workflow from `dist-workspace.toml`; run `dist generate` after
  changing the config and never edit the workflow by hand. The release builds the archives,
  publishes the GitHub release, the Homebrew formula (`publish-homebrew.yml`: `RELEASE_PLEASE_TOKEN` must reach `homebrew-tap`) and the crate
  (`CARGO_REGISTRY_TOKEN`, `publish-crates.yml`). Never bump the version or tag by hand.
- Commit messages follow Conventional Commits. Text in this repository is English.
