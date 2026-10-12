# List the recipes.
[private]
default:
    @just --list --unsorted

# Run all CI jobs.
ci: fmt check clippy test doc build

# Build the workspace.
build:
    cargo build --workspace --all-features --features octocrab/jwt-rust-crypto --locked

# Check formatting.
fmt:
    cargo fmt --all --check

# Check the workspace.
check:
    cargo hack check --workspace --feature-powerset --all-targets --locked

# Run clippy.
clippy:
    cargo hack clippy --workspace --each-feature --all-targets --locked -- --deny warnings

# Run tests.
test:
    cargo hack test --workspace --each-feature --locked

# Run rustdoc.
[env("RUSTDOCFLAGS", "--deny warnings")]
doc:
    cargo hack doc --package octoevents --each-feature --features 'octocrab?/jwt-rust-crypto' --no-deps --locked
    cargo hack doc --package octoevents-derive --each-feature --no-deps --locked
