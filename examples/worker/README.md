# Cloudflare Worker example

The receiver on `wasm32-unknown-unknown`, as a GitHub App's receiver: a
Worker that verifies every request, forwards each envelope its dispatcher is
handed to a [Restate](https://restate.dev) virtual object keyed by the
installation ID, over the crate's wire format from the dispatcher's `always`
tier, then routes it to a handler over a consumer-defined view of the
`installation` payload. A delivery with no installation ID has no object to
go to and is answered 500; the `ping` GitHub sends on creating the webhook is
answered 204 by the receiver before the dispatcher.

An `on_error` observer logs each failed delivery's ID, dispatch error and
source chain to the Worker console before answering 500. Read these in
`wrangler dev` locally or with `npx wrangler tail` for a deployed Worker.

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
`RESTATE_OBJECT_URL`, a plain variable, the ingress URL of the virtual object
envelopes are forwarded to. While it points at nothing, every verified
delivery that reaches the dispatcher answers 500; a refused request is still
401 or 400, and a verified `ping` still 204, so those three are not a sign of
a broken URL. `[vars]` in `wrangler.toml` sets the URL to a local Restate's
ingress, for `wrangler dev`. The secret is never put in `wrangler.toml`; for
local development it goes in a `.dev.vars` file, which `wrangler dev` reads
and `.gitignore` excludes:

```console
echo 'GITHUB_WEBHOOK_SECRET=development-secret' > .dev.vars
npx wrangler dev
```

A deployment uploads the secret once and gives the URL of a Restate the
Worker can reach, since the `[vars]` default is a localhost one:

```console
npx wrangler secret put GITHUB_WEBHOOK_SECRET
npx wrangler deploy --var RESTATE_OBJECT_URL:https://<restate-ingress>/GitHubInstallation
```

(or an `[env.<name>.vars]` block in `wrangler.toml` and `wrangler deploy
--env <name>`).

With `wrangler dev` listening on `http://localhost:8787` and a Restate
server at the `[vars]` URL with a `GitHubInstallation` virtual object whose
`receive` handler accepts the POST, a synthetic `installation.created`
delivery, signed the way GitHub signs, exercises both handlers and is
answered 204; `openssl` computes the HMAC the crate's `Verifier::sign`
would. Without the Restate server the same request is answered 500: the
fetch in `Forward` fails, and since the first error ends the dispatch,
`InstallationLog` never runs. That 500 is the forwarder working as
documented, not the Worker running end to end.

```console
body='{"action":"created","installation":{"id":42,"account":{"login":"octocat"}},"sender":{"id":1,"login":"octocat"}}'
signature=$(printf '%s' "$body" | openssl dgst -sha256 -hmac development-secret | sed 's/^.* //')
curl -i http://localhost:8787 \
  -H 'content-type: application/json' \
  -H 'x-github-event: installation' \
  -H 'x-github-delivery: 72d3162e-cc78-11e3-81ab-4c9367dc0958' \
  -H "x-hub-signature-256: sha256=$signature" \
  --data "$body"
```

Change the secret on either side and the same request is answered 401.

The repository README's `gh webhook forward` line does not exercise this
Worker: it creates a repository webhook, and a repository webhook's
deliveries carry no installation ID, so `Forward` answers each with 500 (and
`installation` is an event only a GitHub App receives). Real deliveries come
from a GitHub App whose webhook URL points at the deployed Worker, or at
`wrangler dev` through a tunnel.

## Editor

The handler's future awaits a JavaScript promise, which is `!Send`; that
compiles for `wasm32`, where the crate's `MaybeSend` bound is empty, and is
an error on the host target. Point rust-analyzer at the target the build
uses, which `.vscode/settings.json` here does for VS Code
(`"rust-analyzer.cargo.target": "wasm32-unknown-unknown"`); other editors set
the same rust-analyzer option. The comment at the top of `src/lib.rs` says
what the diagnostic looks like when the editor is left on the host target.
