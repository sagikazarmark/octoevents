# octoevents

[![ci](https://img.shields.io/github/actions/workflow/status/sagikazarmark/octoevents/dagger.yaml?style=flat-square&label=ci)](https://github.com/sagikazarmark/octoevents/actions/workflows/dagger.yaml)
[![openssf scorecard](https://api.securityscorecards.dev/projects/github.com/sagikazarmark/octoevents/badge?style=flat-square)](https://securityscorecards.dev/viewer/?uri=github.com/sagikazarmark/octoevents)
[![crates.io](https://img.shields.io/crates/v/octoevents?style=flat-square)](https://crates.io/crates/octoevents)
[![docs.rs](https://img.shields.io/docsrs/octoevents?style=flat-square)](https://docs.rs/octoevents)

**Receive and verify GitHub webhook events in Rust.**

## Features

| Feature | Default | Provides |
| --- | --- | --- |
| `http` | yes | `WebhookReceiver` and its builder, `Envelope::from_signed_parts` and `HeaderView` construction from an `http::HeaderMap`, and `ResponseStatus` conversion into `http::StatusCode` |
| `octocrab` | no | `EventHandler` over octocrab's decoded `WebhookEvent`, `Payload` impls for octocrab's per-kind payload structs, `Envelope::decode_event`, and the `Dispatcher` |
| `tower` | no | `tower_service::Service` impl for `WebhookReceiver` |
| `tracing` | no | verify, receive, and dispatch spans without sensitive values |

Enabling `octocrab` makes octocrab's pre-1.0 version part of this crate's
public API; the core (envelope, verification, receiver, `WebhookHandler`,
`PayloadHandler`) does not depend on it.

## Handlers

A handler is a struct whose fields are its dependencies, with a plain
`async fn handle(&self, ..)` and its own error type. Three flavours differ by
what they receive:

```rust
use octoevents::{Envelope, EventKind, EventMeta, PayloadHandler, WebhookHandler};

// The verified envelope: routing metadata plus the exact payload bytes.
struct Persist { /* database pool */ }

impl WebhookHandler for Persist {
    type Error = std::io::Error;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw.len());
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

With the `octocrab` feature, an `EventHandler` receives octocrab's decoded
`WebhookEvent` for any kind, octocrab's payload structs implement `Payload`,
and a `Dispatcher` routes to all of them:

```rust,ignore
Dispatcher::<AppError>::builder()
    .always(Auditor { .. })                     // every delivery, first; not a match
    .on([EventKind::PullRequest, EventKind::Issues], Metrics { .. })
    .on((EventKind::PullRequest, [Action::Opened, Action::Reopened]), Triage { .. })
    .handle_with(Labeler { .. })                // kind from the payload type
    .fallback(Reject)                           // only if nothing matched
    .build()
```

Each handler keeps its own error type; the dispatcher converts them into
`AppError` through `From`. Unmatched deliveries succeed unless a fallback says
otherwise, and a handler that must see the raw bytes before routing (to
persist them, say) wraps the dispatcher as a `WebhookHandler`. The
`dispatcher` example shows the whole shape behind a receiver.

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
