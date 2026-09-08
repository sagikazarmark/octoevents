# Contributing

## Setup

The toolchain, the `wasm32-unknown-unknown` target, [`just`](https://just.systems),
`cargo-watch` and `cargo-hack` come from `devenv shell`; outside it, install
them yourself. The one thing devenv does not ship is a second toolchain: `just
msrv` wants Rust 1.88 beside stable, and the `justfile` says how to provide it.

## Testing

The suite is run through the `justfile` in tiers; `just` lists them.

| Tier | Runs | When |
| --- | --- | --- |
| `just core` | Lib unit tests of both crates, ~1s (`just watch` re-runs it on save) | While editing |
| `just integration` | Every test binary under `tests/` | Before a commit |
| `just docs` | Doctests, the README's included | Before a push |
| `just rustdoc` | `cargo doc` with warnings as errors under no features and under all | After editing a doc comment that links |
| `just full` | Everything the suite has, under `--all-features` | Before a PR |
| `just matrix` | The feature matrix: the suite under all (via `full`); `rustdoc`; `cargo hack --each-feature check --tests`, every test target under each feature alone; then the suite under none and under default | Before a PR |
| `just wasm` | For `wasm32-unknown-unknown`: the lib at both feature extremes, every test target under one `cargo check --tests`, and the Cloudflare Worker example | Before a PR touching handler bounds or a feature gate |
| `just msrv` | `cargo check --all-targets --locked` on Rust 1.88, the declared minimum, at the three feature extremes; skipped with a message when that toolchain is absent | Before a PR touching a dependency or using a newer std API |
| `just lint` | `cargo fmt --check`, then clippy pedantic as errors under all and under no features | Before a PR |
| `just pr` | `full`, `matrix`, `wasm`, `msrv` and `lint` | Before a PR, if in doubt |

A plain `cargo test` is partial. `tracing` and `octocrab` are off by default
and the README's doctests need `tower` (beside `derive`, which is on), so it
skips the README's programs, the fixture corpus tests in `src/decode.rs` and
every test in the three `tracing_*` binaries, `tracing_hygiene` among them,
and passes with nothing to say about them. `just full` is the complete run.

`tests/diagnostics.rs` holds a compile-time diagnostic to its rendered text:
each file under `tests/ui/` must fail to compile with exactly the `.stderr`
beside it. The snapshot changes only when the message, the note, the bound's
signature or rustc's wording does, never for a doc edit above the bound; when
one of those changes on purpose, `TRYBUILD=overwrite cargo test --all-features
--test diagnostics` rewrites the `.stderr`, and the diff is reviewed as the
message is. The harness needs `http` and `tracing`, so it too is in what a
plain `cargo test` skips.

A plain `cargo doc` is partial too: it builds with the default features, and
the front page links to items that exist only under `http`. The comment on the
link definitions at the end of the front page in `src/lib.rs` says how they
resolve without it; `just rustdoc` is where a broken one shows.

The dev profile emits line tables only, so panics and backtraces keep file and
line while the linker copies far less on every incremental test build; the
comment on `[profile.dev]` in `Cargo.toml` says how to get full debuginfo back
for a debugger session.

## Continuous integration

CI is the Dagger `rust` module named in `dagger.toml`, run by
`.github/workflows/dagger.yaml`; it is maintained outside this repository, and
the `justfile` does not drive it. At the module's defaults, `dagger check`
runs `cargo build`, `cargo test`, `cargo clippy`, `cargo doc` (warnings as
errors) and `cargo audit` on one toolchain, Rust 1.98, natively, under the
default features only. Every check `just pr` adds is a plain cargo command, so
aligning the module is a matter of listing them:

| Tier | Commands | The container needs |
| --- | --- | --- |
| `full` | `cargo test --workspace --all-features` | Nothing more |
| `matrix` | `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps` under `--no-default-features` and under `--all-features`; `cargo hack --workspace --each-feature check --tests`; `cargo test --workspace --no-default-features`; `cargo test --workspace` | `cargo-hack` |
| `wasm` | `cargo check --target wasm32-unknown-unknown` under `--no-default-features` and under `--all-features`; `cargo check --tests --target wasm32-unknown-unknown --features octocrab,tower`; `cargo check --manifest-path examples/worker/Cargo.toml --target wasm32-unknown-unknown` | The `wasm32-unknown-unknown` target |
| `msrv` | `cargo check --workspace --all-targets --locked` under `--all-features`, `--no-default-features` and default | A Rust 1.88 toolchain, the `rust-version` in `Cargo.toml` |
| `lint` | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings` under `--all-features` and under `--no-default-features` | `rustfmt` and `clippy`, which it has |

## License

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as in [README.md](README.md#license), without any additional
terms or conditions.
