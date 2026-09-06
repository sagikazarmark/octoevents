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
| `octocrab` | no | `FromEnvelope` impl for octocrab's decoded `WebhookEvent`, `Payload` impls for octocrab's per-kind payload structs, and `Envelope::decode_event` |
| `tower` | no | `tower_service::Service` impl for `WebhookReceiver` |
| `tracing` | no | verify, receive, and dispatch spans without sensitive values |

Enabling `octocrab` makes octocrab's pre-1.0 version part of this crate's
public API; the core (envelope, verification, receiver, both handler
flavours, and the whole `Dispatcher`) does not depend on it.

## Handlers

A handler is a struct whose fields are its dependencies, with a plain
`async fn handle(&self, ..)` and its own error type. Two flavours differ by
what they receive:

```rust
use octoevents::{Envelope, EventKind, EventMeta, PayloadHandler, WebhookHandler};

// The verified envelope: routing metadata plus the exact payload bytes.
// Nothing is decoded, so it runs for every verified delivery, including one
// whose payload nothing can decode.
struct Persist { /* database pool */ }

impl WebhookHandler for Persist {
    type Error = std::io::Error;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw.len());
        Ok(())
    }
}

// The envelope decoded as a type implementing `FromEnvelope`: here a
// `Payload`, a serde view over one kind. The kind comes from the payload
// type, so this handler cannot be registered under the wrong kind. `()` is
// an input too, for a handler routed by kind and action that decodes
// nothing; so is octocrab's `WebhookEvent` with the `octocrab` feature.
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

The receiver accepts a `WebhookHandler`; payload handlers reach it through
a `Dispatcher`. A receiver for one kind and nothing else needs no dispatcher:
a `WebhookHandler` that calls `envelope.decode_payload::<PullRequestNumber>()`
decodes its own view and refuses a delivery of any other kind at the kind.

A `Dispatcher` routes handlers by kind and action through three tiers:
webhook handlers in its `always` and `fallback` tiers, payload handlers by
the kind their payload type declares (and, if wanted, some of its actions),
and payload handlers over any `FromEnvelope` input for the kinds and actions
a matcher selects through `on`:

```rust,ignore
Dispatcher::<AppError>::builder()
    .always(Auditor { .. })                     // every delivery, first, bytes included; not a match
    .on([EventKind::PullRequest, EventKind::Issues], Metrics { .. })      // a consumer `FromEnvelope` view
    .on((EventKind::Installation, Action::Deleted), Revoke { .. })        // over `()`: meta only, nothing decoded
    .on((EventKind::PullRequest, [Action::Opened, Action::Reopened]), Triage { .. }) // over `WebhookEvent`, `octocrab`
    .on_payload(Notify { .. })                  // kind from the payload type, every action
    .on_payload_action([Action::Opened], Labeler { .. })  // kind from the payload type, these actions
    .fallback(Reject)                           // only if nothing matched; bytes included
    .build()
```

Each handler keeps its own error type; the dispatcher converts them into
`AppError` through `From`, and reports a failure as a `DispatchError` that
wraps it with the tier it came from, the delivery's ID, kind and action, and
the source location that registered the failing handler, so a log line leads
straight to the line of code. Nothing is decoded on behalf of `always` or
`fallback`, and each routed handler decodes its own input when its route
runs, so `always`, routes over consumer views or `()`, and a strict
`fallback` all run for a payload octocrab cannot represent; only a handler
over `WebhookEvent` fails on it. A routed handler decodes only when its route
matches, so a payload handler registered for some actions decodes nothing for
a delivery carrying another. Unmatched deliveries succeed unless a fallback says
otherwise. `dispatch` reports an `Outcome` beside the handlers' result:
matched, or unmatched with the kind known or unknown to the route table. The
receiver sees only the result; a handler wrapping the dispatcher reads the
outcome to forward or dead-letter an unmatched delivery, bytes included, or
to reject kinds it never registered while tolerating a new action on a kind
it handles. A tier can continue or fail but never skip, so a handler that
decides whether a delivery is routed at all (persist first, answer a
redelivery of a stored delivery ID with success) is that same wrapper. The
`dispatcher` example shows the whole shape behind a receiver; the `worker`
example forwards each envelope from the `always` tier and routes a payload
handler without octocrab on Cloudflare Workers.

Closures work for both flavours, and `Arc<H>` is a handler of either flavour
when `H` is. Annotate the parameters a closure's body uses
(`|envelope: Envelope|`, `|meta: EventMeta, pr: PullRequestNumber|`):
registration is bound on the handler trait rather than on `Fn`, so rustc does
not read their types off the call. Always state the error type
(`Ok::<_, E>(())`): a bare `Ok(())` fails with E0282 on the receiver path and
E0283 (ambiguous `From`) on the dispatcher path.

## Delivery semantics

GitHub signs no timestamp, so the crate provides no replay protection: treat
`EventMeta::delivery_id` as an idempotency key.

GitHub does not retry a failed delivery on its own, and it abandons a request
after 10 seconds (30 on GitHub Enterprise Server). Persist or forward an
envelope before returning and process it afterwards. With a dispatcher, that
work goes in a webhook handler wrapping `dispatch`: it stores the envelope,
bytes included, before anything is routed, and answers a redelivery of a
stored delivery ID with success without routing it. A forwarder that never
needs to skip fits the `always` tier, which receives the same envelope before
routing.

The receiver answers a failed delivery with a bare 500: the response is
GitHub's delivery record, not a log, so the receiver places no `Display` bound
on the error type and never reads it. To see why a delivery failed, register
an observer with `on_error`. It receives the event meta and the handler's
error before the 500 is answered, with no `Error`, `Display` or `Debug` bound
on the error type, so a boxed `dyn Error` is as observable as a named enum.
With a `Dispatcher` inside, the error is a `DispatchError` naming the tier,
the delivery, and the line that registered the failing handler, and its
source is the application error:

```rust,ignore
use std::error::Error as _;

use octoevents::{DispatchError, EventMeta, WebhookReceiverBuilder};

let receiver = WebhookReceiverBuilder::new(verifier)
    .on_error(|meta: &EventMeta, error: &DispatchError<AppError>| {
        eprintln!("{error}");
        let mut cause = error.source();
        while let Some(error) = cause {
            eprintln!("  caused by: {error}");
            cause = error.source();
        }
    })
    .build(dispatcher);
```

A failed delivery then logs, before the 500:

```text
delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the always tier at the handler registered at src/main.rs:12:6
  caused by: database is down
```

The observer runs only when a handler ran and failed: a request the receiver
refused (bad signature, missing header, wrong content type, body over the
limit) is a status code, and a short-circuited `ping` reaches no handler.
Annotate the error parameter when the body calls methods on it; the error
type is fixed by `build`, later in the chain.

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
