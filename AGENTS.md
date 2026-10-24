# AGENTS.md

`heavy` is a Rust crate: a library (`src/lib.rs`, the pool of `flock` slots) and a command line
(`src/main.rs`) that runs a command while holding a slot.

- Toolchain and tools come from `mise.toml`: `mise exec -- cargo test`, `cargo fmt`,
  `cargo clippy --all-targets -- -D warnings`. All three must pass before a push.
- The tests run the real binary against real processes (`tests/cli.rs`): queueing, `kill -9`,
  nesting, exit codes. A change to the behaviour starts with a test there.
- Releases: `dist` (cargo-dist) generates `.github/workflows/release.yml` from
  `dist-workspace.toml`; run `dist generate` after changing the config and never edit the workflow
  by hand. A release is a tag `vX.Y.Z` pushed after the version in `Cargo.toml` is bumped.
- Commit messages follow Conventional Commits. Text in this repository is English.
