# The test suite in tiers, cheapest first; CONTRIBUTING.md says which to run
# when and what a plain `cargo test` leaves out. The four test tiers pass
# `--all-features` so that nothing is skipped; matrix, wasm, msrv and lint then
# vary the feature set, the target and the toolchain on purpose.
#
# The matrix tier runs the suite at the three feature extremes, and between
# them `cargo hack --each-feature` (from devenv) type-checks every test target
# under each feature alone, where an import gated on one feature but used under
# another goes stale unseen by the extremes; the suite itself runs only at the
# extremes because nine runs of it would be slow for what a check catches. The
# rustdoc tier is where a front-page link to an item behind `http` breaks under
# `--no-default-features`; a doctest run does not resolve links.
#
# The wasm tier checks the lib at both feature extremes, then every test target
# under one command (the unit test modules on tokio gate themselves off the
# target; `tests/wasm_handlers.rs` is never run and does not compile natively),
# then the Cloudflare Worker example. The lint tier runs clippy at both feature
# extremes, with the workspace's pedantic lints as errors.
#
# The msrv tier needs a Rust 1.88 toolchain, which devenv (stable only) does not
# ship. Either `rustup toolchain install 1.88`, which `cargo +1.88` then selects,
# or put a 1.88 toolchain's `bin` first on PATH: a bare `cargo` drives whichever
# `rustc` PATH finds, so the probe wants both. Without one the tier says so and
# passes, and the probe does not let rustup install the toolchain on its own.
#
# CI runs what the Dagger module in `dagger.toml` runs, not this file; the
# "Continuous integration" section of CONTRIBUTING.md says which tiers it
# should be aligned with. Every tier is a plain cargo command so that wiring
# one is a line.

# List the tiers.
default:
  @just --list --unsorted

# Lib unit tests of both crates, ~1s: the loop while editing.
core:
  cargo test --workspace --lib --all-features

# Re-run the core tier on every save.
watch:
  cargo watch --clear --shell 'just core'

# Every integration test binary under tests/.
integration:
  cargo test --workspace --test '*' --all-features

# Doctests, the README's included.
docs:
  cargo test --workspace --doc --all-features

# Everything the suite has, under one feature set.
full:
  cargo test --workspace --all-features

# Rustdoc at both feature extremes, warnings as errors: docs.rs builds with every feature, a consumer with none.
rustdoc:
  RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --no-default-features
  RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features

# The feature matrix: the suite under all (via full), rustdoc under none and all, every test target under each feature alone, then the suite under none and default.
matrix: full rustdoc
  cargo hack --workspace --each-feature check --tests
  cargo test --workspace --no-default-features
  cargo test --workspace

# The wasm32 target: lib checks at both extremes, every test target under one command, then the Worker example.
wasm:
  cargo check --target wasm32-unknown-unknown --no-default-features
  cargo check --target wasm32-unknown-unknown --all-features
  cargo check --tests --target wasm32-unknown-unknown --features octocrab,tower
  cargo check --manifest-path examples/worker/Cargo.toml --target wasm32-unknown-unknown

# The declared minimum, Rust 1.88, at the three feature extremes with every target, against the lockfile; skipped with a message without that toolchain.
msrv:
  #!/usr/bin/env sh
  set -eu
  if RUSTUP_AUTO_INSTALL=0 cargo +1.88 --version >/dev/null 2>&1; then
    cargo='cargo +1.88'
  elif cargo --version | grep -q '^cargo 1\.88\.' && rustc --version | grep -q '^rustc 1\.88\.'; then
    cargo=cargo
  else
    echo 'msrv: skipped, no Rust 1.88 toolchain; `rustup toolchain install 1.88`, or put one first on PATH' >&2
    exit 0
  fi
  set -x
  $cargo check --workspace --all-targets --locked --all-features
  $cargo check --workspace --all-targets --locked --no-default-features
  $cargo check --workspace --all-targets --locked

# Formatting, then clippy pedantic as errors under all and under no features.
lint:
  cargo fmt --all --check
  cargo clippy --workspace --all-features --all-targets -- -D warnings
  cargo clippy --workspace --no-default-features --all-targets -- -D warnings

# What a PR should pass: full, matrix, wasm, msrv and lint.
pr: matrix wasm msrv lint
