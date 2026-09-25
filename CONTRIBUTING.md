# Contributing

[Documentation index](README.md#documentation)

## Start from a fresh clone

For a local Cargo loop, install [Rust through rustup](https://rustup.rs/) with
the current stable toolchain and the `rustfmt` and `clippy` components:

```console
rustup toolchain install stable --component rustfmt --component clippy
git clone https://github.com/sagikazarmark/octoevents.git
```

Work from the cloned repository's root. The declared MSRV is in `Cargo.toml`;
the current lockfile can have newer dependency requirements, so use current
stable for the normal contributor loop.

```console
cargo test --workspace --all-features --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets --locked -- -D warnings
```

These use local tools. A fast core-only loop is
`cargo test --workspace --no-default-features --locked`. Run a single example
with `cargo test --example quickstart --features tower --locked`.

### Containerized CI checks

Install [Nix](https://nixos.org/download/) and
[devenv](https://devenv.sh/getting-started/), and start a Docker-compatible
container runtime that Dagger can use. The repository's development shell
supplies Rust, the wasm target, auxiliary Cargo tools and Dagger's configured
release, plus Python and lychee for documentation checks. Enter it from the repository root:

```console
devenv shell
dagger check -l
dagger check
```

The generated `.github/workflows/dagger.yaml` runs `dagger check` with its
pinned Dagger release. `dagger.toml` selects the Rust container; the
`workspace.metadata.dagger` entries in `Cargo.toml` configure cargo-hack.
Check uses the feature powerset; tests, Clippy and rustdoc use each-feature
runs (including no features, defaults and all features). Rustdoc denies
warnings. Test runs include workspace doctests and enabled `test = true`
examples. The current default Dagger configuration does not add a separate
MSRV or wasm matrix. Do not edit generated workflows by hand.

For an optional Worker check, install the target and use the standalone
package's manifest:

```console
rustup target add wasm32-unknown-unknown
cargo check --manifest-path examples/worker/Cargo.toml --target wasm32-unknown-unknown --locked
```

This is a compile check, not a wasm runtime test. Deployment prerequisites
and commands are in the [Worker guide](examples/worker/README.md).

## Change documentation

Use the README as the landing page, `docs/` for task-oriented guides, and
rustdoc for API contracts. Keep terminology consistent with
[CONTEXT.md](CONTEXT.md). Give public APIs a summary and document applicable
errors and panics. `missing_docs` and Clippy's pedantic documentation lints
are enabled; rustdoc checks intra-doc links.

Run these after changing prose or snippets:

```console
cargo test --workspace --all-features --doc --locked
cargo test --all-features --test readme_testing --examples --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --features octocrab/jwt-rust-crypto --no-deps --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-default-features --no-deps --locked
python3 scripts/check-docs.py
lychee --no-progress --include-fragments README.md CONTRIBUTING.md 'docs/*.md' examples/worker/README.md tests/fixtures/README.md
```

Install [lychee](https://lychee.cli.rs/) for the last command. The Python
check uses only Python's standard library. It runs the README's `cargo add`
commands in a temporary consumer crate and builds the exact program, then
checks it against this checkout. It also checks snippet consistency.
It needs registry access to resolve the current published dependencies.

Coverage is split deliberately:

- README and guide Rust blocks are included as doctests; ordinary blocks run,
  `no_run` blocks compile, and `compile_fail` blocks must fail compilation.
- The two test-function snippets in the integration guide are checked against
  executable tests in `tests/readme_testing.rs`. Keep their bodies identical.
- The observability example tests actual subscriber output for refusals and
  errors. CI also runs its standalone diagnostic command.
- README installation commands are checked through the external consumer,
  not by rustdoc. Markdown links/anchors are checked by lychee, not rustdoc.
- Shell procedures requiring an administered GitHub repository, a deployed
  Worker or a Restate server require manual end-to-end validation. Do not
  equate a compile check with that validation.

The dedicated documentation workflow checks links, the external consumer and
guide examples. Open `target/doc/octoevents/index.html` to inspect local API
docs. docs.rs enables all features and `doc_cfg` annotations for gated APIs.
If a new snippet requires another feature, update its doctest gate and test
that feature combination. New guides with Rust blocks must be added to the
documentation includes in `src/lib.rs`.

## Compatibility and releases

[GitHub Releases](https://github.com/sagikazarmark/octoevents/releases) is the
release-note destination. When preparing an upgrade, compare the source and
destination releases, including the wire-format contract if envelopes cross
services or remain in storage.

For a change that breaks an API, the wire format, or compiler compatibility,
include migration instructions in the PR description for the release notes.
Name affected consumers, old/new forms, and producer/consumer coordination.
For incompatible octocrab changes, name the exact supported release line.
Use `cargo add` for installation instructions and `latest` API links so docs
do not need updating for each release. Keep exact versions only where they
explain compatibility requirements or version-specific behavior. The
quickstart check tests both current published dependencies and the checkout.
When an upcoming breaking change needs a different quickstart, keep the
released guide intact until publication and demonstrate the new API in a
tested repository example. The derive crate follows the main crate in
lockstep and is published first.

Before proposing an MSRV claim for a release, verify the intended dependency
resolution on that compiler. A `rust-version` field alone does not demonstrate
that the newest transitive dependency resolution builds on it.
