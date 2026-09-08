# The test suite in tiers, cheapest first; CONTRIBUTING.md says which to run
# when. `tracing` and `octocrab` are off by default and the README's doctests
# need `tower`, so a plain `cargo test` skips the README, the fixture corpus
# and the three `tracing_*` binaries; every tier here passes `--all-features`
# so nothing is left out.
#
# The wasm tier checks the lib at both feature extremes, then builds the two
# compile-only test files; `tests/wasm_handlers.rs` is never run and does not
# compile natively. The lint tier runs clippy at both feature extremes, with
# the workspace's pedantic lints as errors, so a cfg-gated import cannot go
# stale unnoticed.

# List the tiers.
default:
    @just --list --unsorted

# Lib unit tests of both crates, ~1s: the loop while editing.
core:
    cargo test --workspace --lib --all-features

# Re-run the core tier on every save.
watch:
    cargo watch --clear --exec 'test --workspace --lib --all-features'

# Every integration test binary under tests/.
integration:
    cargo test --workspace --test '*' --all-features

# Doctests, the README's included.
docs:
    cargo test --workspace --doc --all-features

# Everything the suite has, one feature set: what CI's test step must match.
full:
    cargo test --workspace --all-features

# The suite at the three feature extremes: all (via full), none, default.
matrix: full
    cargo test --workspace --no-default-features
    cargo test --workspace

# The wasm32 target: lib checks at both extremes, then the compile-only test files.
wasm:
    cargo check --target wasm32-unknown-unknown --no-default-features
    cargo check --target wasm32-unknown-unknown --all-features
    cargo build --test wasm_handlers --target wasm32-unknown-unknown --features octocrab,tower
    cargo build --test handler_adapter --target wasm32-unknown-unknown

# Formatting, then clippy pedantic as errors under all and under no features.
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-features --all-targets -- -D warnings
    cargo clippy --workspace --no-default-features --all-targets -- -D warnings

# What a PR should pass: full, matrix, wasm and lint.
all: matrix wasm lint
