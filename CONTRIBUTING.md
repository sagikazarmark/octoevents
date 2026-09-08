# Contributing

## Setup

The toolchain, the `wasm32-unknown-unknown` target, [`just`](https://just.systems)
and `cargo-watch` come from `devenv shell`; outside it, install them yourself.

## Testing

The suite is run through the `justfile` in tiers; `just` lists them.

| Tier | Runs | When |
| --- | --- | --- |
| `just core` | Lib unit tests of both crates, ~1s (`just watch` re-runs it on save) | While editing |
| `just integration` | Every test binary under `tests/` | Before a commit |
| `just docs` | Doctests, the README's included | Before a push |
| `just full` | Everything, under `--all-features`: what CI's test step must match | Before a PR |
| `just matrix` | The suite at the three feature extremes: none, default, all | Before a PR |
| `just wasm` | The lib and the compile-only test files for `wasm32-unknown-unknown` | Before a PR touching handler bounds or a feature gate |
| `just lint` | `cargo fmt --check`, then clippy pedantic as errors under all and under no features | Before a PR |
| `just all` | `full`, `matrix`, `wasm` and `lint` | Before a PR, if in doubt |

A plain `cargo test` is partial. `tracing` and `octocrab` are off by default
and the README's doctests need `tower`, so it skips the README's programs, the
fixture corpus tests in `src/decode.rs` and every test in the three
`tracing_*` binaries, and passes with nothing to say about them. `just full`
is the complete run.

The dev profile emits line tables only (`[profile.dev]` in `Cargo.toml`):
panics and backtraces keep file and line, the edit-to-green cycle is a quarter
to a half shorter than under full debuginfo, and the test binaries are a third
of the size. For a debugger session, `CARGO_PROFILE_DEV_DEBUG=2 cargo test ...`
restores full debuginfo for that build.

## License

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as in [README.md](README.md#license), without any additional
terms or conditions.
