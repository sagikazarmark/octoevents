# octoevents

[![ci](https://img.shields.io/github/actions/workflow/status/sagikazarmark/octoevents/dagger.yaml?style=flat-square&label=ci)](https://github.com/sagikazarmark/octoevents/actions/workflows/dagger.yaml)
[![openssf scorecard](https://api.securityscorecards.dev/projects/github.com/sagikazarmark/octoevents/badge?style=flat-square)](https://securityscorecards.dev/viewer/?uri=github.com/sagikazarmark/octoevents)
[![crates.io](https://img.shields.io/crates/v/octoevents?style=flat-square)](https://crates.io/crates/octoevents)
[![docs.rs](https://img.shields.io/docsrs/octoevents?style=flat-square)](https://docs.rs/octoevents)

**Receive and verify GitHub webhook events in Rust.**

## Features

| Feature | Default | Provides |
| --- | --- | --- |
| `http` | yes | `WebhookReceiver` and its builder, `Envelope::from_signed_headers` and `HeaderView` construction from an `http::HeaderMap`, and `ResponseStatus` conversion into `http::StatusCode` |
| `octocrab` | no | `EventHandler` over octocrab's decoded `WebhookEvent`, `Payload` impls for octocrab's per-kind payload structs, `Envelope::decode_event`, and `Dispatcher::on` |
| `tower` | no | `tower_service::Service` impl for `WebhookReceiver` |
| `tracing` | no | verify, receive, and dispatch spans without sensitive values |

Enabling `octocrab` makes octocrab's pre-1.0 version part of this crate's
public API; the core (envelope, verification, receiver, `WebhookHandler`,
`MetaHandler`, `PayloadHandler`, and the `Dispatcher` apart from `on`) does
not depend on it.

## Handlers

A handler is a struct whose fields are its dependencies, with a plain
`async fn handle(&self, ..)` and its own error type. Four flavours differ by
what they receive:

```rust
use octoevents::{Envelope, EventKind, EventMeta, MetaHandler, PayloadHandler, WebhookHandler};

// The verified envelope: routing metadata plus the exact payload bytes.
struct Persist { /* database pool */ }

impl WebhookHandler for Persist {
    type Error = std::io::Error;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw.len());
        Ok(())
    }
}

// The metadata alone: no bytes, no decode, so it runs for every verified
// delivery, including one whose payload nothing can decode.
struct Dedup { /* seen delivery IDs */ }

impl MetaHandler for Dedup {
    type Error = std::io::Error;

    async fn handle(&self, meta: EventMeta) -> Result<(), Self::Error> {
        println!("{} {} from {:?}", meta.delivery_id, meta.kind, meta.sender);
        Ok(())
    }
}

// One kind's decoded payload. The kind comes from the payload type, so this
// handler cannot be registered under the wrong kind.
#[derive(serde::Deserialize)]
struct PullRequestNumber { number: u64 }
octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);

struct Labeler { /* GitHub API client */ }

impl PayloadHandler<PullRequestNumber> for Labeler {
    type Error = std::io::Error;

    async fn handle(&self, meta: EventMeta, pr: PullRequestNumber) -> Result<(), Self::Error> {
        println!("{}: label PR #{}", meta.delivery_id, pr.number);
        Ok(())
    }
}
```

The receiver accepts a `WebhookHandler`; every other flavour converts into one
with `into_webhook_handler()`.

A `Dispatcher` routes handlers by kind and action: webhook handlers in its raw
tier, meta handlers in its `always` and `fallback` tiers, payload handlers by
the kind their payload type declares (and, if wanted, some of its actions),
and, with the `octocrab` feature, `EventHandler`s over octocrab's decoded
`WebhookEvent` for any kind through `on`:

```rust,ignore
Dispatcher::<AppError>::builder()
    .always_raw(Persist { .. })                 // every delivery, first, bytes included; not a match
    .always(Auditor { .. })                     // every delivery, after the raw tier; not a match
    .on([EventKind::PullRequest, EventKind::Issues], Metrics { .. })                 // `octocrab`
    .on((EventKind::PullRequest, [Action::Opened, Action::Reopened]), Triage { .. }) // `octocrab`
    .on_payload(Notify { .. })                  // kind from the payload type, every action
    .on_payload_action([Action::Opened], Labeler { .. })  // kind from the payload type, these actions
    .fallback(Reject)                           // only if nothing matched
    .build()
```

Each handler keeps its own error type; the dispatcher converts them into
`AppError` through `From`. Raw, meta and payload handlers never decode with
octocrab, so `always_raw`, `always`, payload routes, and a strict `fallback`
all run for a payload octocrab cannot represent; only the first event handler
reached decodes it, once. A routed handler decodes only when its route
matches, so a payload handler registered for some actions decodes nothing for
a delivery carrying another. Unmatched deliveries succeed unless a fallback
says otherwise. A handler that must see the raw bytes before routing (to
persist them, say) goes in `always_raw`: it runs before every other tier, and
its failure keeps the delivery from being routed. The `dispatcher` example
shows the whole shape behind a receiver; the `worker` example forwards each
envelope from the raw tier and routes a payload handler without octocrab on
Cloudflare Workers.

Closures work for every flavour; annotate their parameter types
(`|envelope: Envelope|`) and, where nothing else fixes it, the error type
(`Ok::<_, E>(())`).

## Quick start

The fastest way to see a verified envelope is to point real GitHub deliveries
at the bundled Axum example with
[`gh webhook forward`](https://docs.github.com/en/webhooks/testing-and-troubleshooting-webhooks/using-the-github-cli-to-forward-webhooks-for-testing),
which creates a temporary webhook on a repository you administer and forwards
its deliveries (exact body bytes and headers, signature included) to
localhost.

In one terminal, start the example receiver:

```console
GITHUB_WEBHOOK_SECRET=development-secret cargo run --example axum --features tower
```

In another, forward `issues` events from your repository, signed with the same
secret:

```console
gh extension install cli/gh-webhook
gh webhook forward --repo=<owner>/<repo> --events=issues \
  --url=http://127.0.0.1:3000/webhook --secret=development-secret
```

Open or close an issue in the repository, and the receiver prints the delivery
ID and kind of each verified envelope. Change the secret on either side and
the same delivery is refused with 401 instead: the forwarded bytes no longer
match their signature. The webhook `gh` creates delivers JSON, which is the
content type this crate requires.

If forwarding fails with "you do not have access to this feature", the usual
cause is a token that cannot create webhooks on the repository; for a
fine-grained personal access token, grant the "Webhooks" repository
permission (read and write).

To see typed routing instead, run the `dispatcher` example the same way and
forward `pull_request` events:

```console
GITHUB_WEBHOOK_SECRET=development-secret cargo run --example dispatcher --features tower,octocrab
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
