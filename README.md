# octoevents

[![ci](https://img.shields.io/github/actions/workflow/status/sagikazarmark/octoevents/dagger.yaml?style=flat-square&label=ci)](https://github.com/sagikazarmark/octoevents/actions/workflows/dagger.yaml)
[![openssf scorecard](https://api.securityscorecards.dev/projects/github.com/sagikazarmark/octoevents/badge?style=flat-square)](https://securityscorecards.dev/viewer/?uri=github.com/sagikazarmark/octoevents)
[![crates.io](https://img.shields.io/crates/v/octoevents?style=flat-square)](https://crates.io/crates/octoevents)
[![docs.rs](https://img.shields.io/docsrs/octoevents?style=flat-square)](https://docs.rs/octoevents/latest/octoevents/)

**Receive and verify GitHub webhooks in Rust.**

A receiver turns an untrusted HTTP request into an `Envelope`: exact payload bytes and routing metadata.
An optional `Dispatcher` routes envelopes to handlers by event kind and action.
Handlers can read metadata, decode a small serde view, or use octocrab's payload types.

The core performs no network I/O and builds for `wasm32-unknown-unknown`.
Mount the receiver on axum, Cloudflare Workers, or another HTTP transport.
Your application owns paths, methods, authorization, persistence and recovery.
The crate does not send webhooks or request GitHub redelivery.

## Status and compatibility

octoevents is a **pre-1.0** library.
Public APIs and the envelope wire format may change incompatibly in a minor release;
review the [release notes](https://github.com/sagikazarmark/octoevents/releases) before upgrading.
This repository does not promise an LTS/support window.

The declared minimum Rust version is the `rust-version` in [Cargo.toml](Cargo.toml).
Your resolved dependencies may require a newer compiler;
retain your lockfile and check it with the toolchain you deploy.
MSRV changes should be called out in release notes; do not assume a compiler-support window from the crate version.

Using the optional octocrab models couples your handlers to its compatible release line;
see the [integration guide](docs/guide.md#octocrab-payloads).
An incompatible octocrab upgrade, including a pre-1.0 minor bump, changes those public types.
For services exchanging stored envelopes, also review the
[wire-format compatibility contract](https://docs.rs/octoevents/latest/octoevents/struct.Envelope.html#wire-format).

## Quickstart

Create an application with `cargo new webhook-demo`, then work in that directory.
Add the dependencies:

```console
cargo add axum
cargo add octoevents --features tower
cargo add tokio --features macros,net,rt-multi-thread
```

Put this in `src/main.rs`.
It prints a thank-you when an issue is opened; it does not post a comment to GitHub.

```rust,no_run
use axum::{Router, routing::post_service};
use octoevents::{Action, BoxError, Dispatcher, Envelope, EventKind, Verifier, WebhookReceiverBuilder, WebhookSecret};

async fn thank(envelope: Envelope) -> Result<(), BoxError> {
    let sender = envelope.meta.sender.map(|s| s.login).unwrap_or_default();
    println!("Thank you for your contribution, @{sender}! :)");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;
    let dispatcher = Dispatcher::builder()
        .on((EventKind::Issues, Action::Opened), thank)
        .build();
    let verifier = Verifier::new(WebhookSecret::new(secret));
    let webhook = WebhookReceiverBuilder::new(verifier).build(dispatcher);

    let app = Router::new().route("/webhook", post_service(webhook));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

```console
GITHUB_WEBHOOK_SECRET=development-secret cargo run
```

The verifier authenticates the exact body bytes.
The dispatcher selects `issues.opened`; `Envelope` gives the handler the metadata and those bytes.
`BoxError` lets the handler return any compatible error with `?`.

Successful, unmatched and default `ping` deliveries receive 204.
A handler or input-decode error receives 500; refused requests receive 400, 401 or 413.
Responses have no body.
Enable [observability](docs/observability.md) to see errors and refusals.

**Next:** [forward a real GitHub delivery](docs/guide.md#try-it), or
[test without GitHub](docs/guide.md#testing-without-github).
From a repository clone the same server is available as `cargo run --example quickstart --features tower`.

## Security essentials

- Signatures authenticate **payload bytes**, not the delivery ID, event name, or target headers.
  Routing and successful decoding are not authorization.
- Delivery-ID deduplication handles GitHub redelivery, not adversarial replay
  of a captured payload under another ID.
- The default 25 MiB body limit bounds accumulated payload length, not total process memory.
  Configure transport timeouts and concurrency separately.
- GitHub does not automatically redeliver failures.
  A production application needs a [durable receipt and recovery policy](docs/recovery.md).

Read the [security and authorization guide](docs/security.md) for the trust boundaries, secret rotation,
and trusted internal forwarding.

## Cargo features

| Feature | Default | Purpose |
| --- | --- | --- |
| `http-body` | yes | Receive a streaming `http::Request` |
| `derive` | yes | Declare a serde view's kind with `#[derive(Payload)]` |
| `tower` | no | Mount the receiver as a Tower service; enables `http-body` |
| `octocrab` | no | Decode octocrab's webhook payload types |
| `tracing` | no | Receive, verify and dispatch spans; failed-delivery events |

With no features, verification, envelopes, dispatch and `receive_bytes` are available.
The [API feature reference](https://docs.rs/octoevents/latest/octoevents/#features) defines the full contracts
and octocrab backend requirements.

## Documentation

| I want to… | Start here |
| --- | --- |
| Understand the types and lifecycle | [API concepts](https://docs.rs/octoevents/latest/octoevents/#concepts), [glossary](docs/glossary.md) |
| Integrate handlers, routing or another transport | [Integration guide](docs/guide.md) |
| Migrate from Probot | [Migration table](docs/guide.md#migrating-from-probot) |
| Diagnose a refused or failed delivery | [Observability](docs/observability.md) |
| Persist, deduplicate and recover work | [Durable receipt and recovery](docs/recovery.md) |
| Review authorization or rotate secrets | [Security](docs/security.md) |
| Run on Cloudflare Workers | [Worker guide](examples/worker/README.md) |
| Upgrade | [Release notes](https://github.com/sagikazarmark/octoevents/releases) |

Executable examples, in reading order: [quickstart](examples/quickstart.rs), [one struct handler](examples/axum.rs),
[dispatcher](examples/dispatcher.rs), [policy seam](examples/policy_seam.rs),
[observability](examples/observability.rs).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you,
as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
