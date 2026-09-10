# Dagger runs the PR gate, locally and in CI. The upstream Rust module lives
# in dagger.toml; its cargo-hack matrices live in Cargo.toml. Target-selection
# flags need API calls until `dagger check` supports function arguments.
# Keep direct Cargo commands for the fast edit-to-green loop.

# List the tiers.
default:
    @just --list --unsorted

# Lib unit tests of both crates: the loop while editing.
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

# Native feature powerset, tests, examples and rustdoc.
matrix:
    dagger check rust:test rust:doc
    dagger api call rust check --all-targets

# wasm32 feature powerset and non-Send test coverage, then the Worker example.
wasm:
    dagger api call rust --targets wasm32-unknown-unknown check --exclude octoevents-derive --lib --test '*' --target wasm32-unknown-unknown
    dagger api call rust --targets wasm32-unknown-unknown container with-exec --args cargo,check,--manifest-path,examples/worker/Cargo.toml,--target,wasm32-unknown-unknown,--locked combined-output

# The declared minimum Rust version, installed by Dagger, across the powerset.
msrv:
    dagger api call rust --toolchain msrv check --all-targets

# Formatting and clippy across features, warnings as errors.
lint:
    dagger check rust:fmt
    dagger api call rust clippy --all-targets --deny warnings

# Both CI workflows' checks, including the dependency audit and generated files.
pr: wasm msrv
    dagger check
    dagger api call rust check --all-targets
    dagger api call rust clippy --all-targets --deny warnings
