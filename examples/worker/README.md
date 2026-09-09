# Cloudflare Worker example

The receiver on `wasm32-unknown-unknown`: a Worker that verifies every
request, forwards each verified envelope to a [Restate](https://restate.dev)
virtual object over the crate's wire format from the dispatcher's `always`
tier, then routes it to a handler over a consumer-defined view of the
`installation` payload.

This is a package of its own, not a member of the repository's workspace:
it has a `cdylib` target, its own lockfile and a wasm-only dependency
(`worker`), so `cargo run --example worker` from the repository root does not
find it. Every command below runs from this directory.

## Build

The `wasm32-unknown-unknown` target and
[`worker-build`](https://github.com/cloudflare/workers-rs) (which
`wrangler` runs through `[build]` in `wrangler.toml`):

```console
rustup target add wasm32-unknown-unknown
cargo install worker-build
```

A type check needs neither `wrangler` nor `worker-build`, and is what the
repository's `just wasm` tier runs:

```console
cargo check --target wasm32-unknown-unknown
```

## Run

The Worker reads two bindings: `GITHUB_WEBHOOK_SECRET`, a secret, and
`RESTATE_OBJECT_URL`, a plain variable set under `[vars]` in `wrangler.toml`
(the ingress URL of the virtual object envelopes are forwarded to; every
request answers 500 while it points at nothing). The secret is never put in
`wrangler.toml`; for local development it goes in a `.dev.vars` file, which
`wrangler dev` reads and `.gitignore` excludes, and for a deployment it is
uploaded once:

```console
echo 'GITHUB_WEBHOOK_SECRET=development-secret' > .dev.vars
npx wrangler dev
```

```console
npx wrangler secret put GITHUB_WEBHOOK_SECRET
npx wrangler deploy
```

With `wrangler dev` listening on `http://localhost:8787`, the README's
`gh webhook forward` line forwards real deliveries to it:

```console
gh webhook forward --repo=<owner>/<repo> --events=installation,issues \
  --url=http://localhost:8787 --secret=development-secret
```

## Editor

The handler's future awaits a JavaScript promise, which is `!Send`; that
compiles for `wasm32`, where the crate's `MaybeSend` bound is empty, and is
an error on the host target. Point rust-analyzer at the target the build
uses, which `.vscode/settings.json` here does for VS Code
(`"rust-analyzer.cargo.target": "wasm32-unknown-unknown"`); other editors set
the same rust-analyzer option. The comment at the top of `src/lib.rs` says
what the diagnostic looks like when the editor is left on the host target.
