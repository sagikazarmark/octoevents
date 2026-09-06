# octoevents

[![ci](https://img.shields.io/github/actions/workflow/status/sagikazarmark/octoevents/dagger.yaml?style=flat-square&label=ci)](https://github.com/sagikazarmark/octoevents/actions/workflows/dagger.yaml)
[![openssf scorecard](https://api.securityscorecards.dev/projects/github.com/sagikazarmark/octoevents/badge?style=flat-square)](https://securityscorecards.dev/viewer/?uri=github.com/sagikazarmark/octoevents)
[![crates.io](https://img.shields.io/crates/v/octoevents?style=flat-square)](https://crates.io/crates/octoevents)
[![docs.rs](https://img.shields.io/docsrs/octoevents?style=flat-square)](https://docs.rs/octoevents)

**Receive and verify GitHub webhook events in Rust.**

A complete receiver that labels every opened issue and audits every delivery:

```rust,no_run
use axum::{Router, routing::post_service};
use octoevents::{
    Action, DecodeError, DispatchError, Dispatcher, Envelope, EventKind, EventMeta, Secret,
    Verifier, WebhookReceiverBuilder,
};

/// The one error every handler returns. The dispatcher decodes payloads on
/// the handlers' behalf and reports a payload that does not fit through it.
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

/// The fields this bot reads off an `issues` payload, and nothing else.
#[derive(serde::Deserialize)]
struct IssueOpened {
    issue: Issue,
}

#[derive(serde::Deserialize)]
struct Issue {
    number: u64,
    title: String,
}

octoevents::impl_payload!(IssueOpened => EventKind::Issues);

/// Runs for `issues.opened`, with the payload decoded as `IssueOpened`.
async fn label(meta: EventMeta, payload: IssueOpened) -> Result<(), AppError> {
    println!("{}: label #{} '{}'", meta.delivery_id, payload.issue.number, payload.issue.title);
    Ok(())
}

/// Runs for every verified delivery, bytes included.
async fn audit(envelope: Envelope) -> Result<(), AppError> {
    println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw.len());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;

    let dispatcher = Dispatcher::<AppError>::builder()
        .always(audit)
        .on_payload_action([Action::Opened], label)
        .build();

    let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new(secret)))
        .on_error(|_: &EventMeta, error: &DispatchError<AppError>| {
            eprintln!("{error}: {}", error.source);
        })
        .build(dispatcher);

    let app = Router::new().route("/webhook", post_service(webhook));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

```toml
[dependencies]
axum = "0.8"
octoevents = { version = "0.2", features = ["tower"] }
serde = { version = "1", features = ["derive"] }
thiserror = "2"
tokio = { version = "1", features = ["macros", "net", "rt-multi-thread"] }
```

The receiver verifies `X-Hub-Signature-256` against the exact body bytes with
the secret (refusing an unsigned request before it reads the body) and
answers GitHub with a bare status: 204 when the handler succeeded, 500 when
it failed, 401, 400 or 413 for a request that never reached one. The
dispatcher routes each verified envelope by kind and action: `audit` runs for
every delivery, `label` for `issues.opened` only, with the payload decoded
as the view it asked for. The `on_error` observer is where a failure becomes
visible; without it a failed delivery is a silent 500. The observer prints
where (the tier and the line that registered the failing handler) and why
(the handler's own error); for a payload that does not fit the view, that is:

```text
delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the route tier at the handler registered at src/main.rs:47:10: payload could not be decoded
```

The serde error naming the field is one step further down the error's
`source()` chain, which the `dispatcher` example walks.

## Features

| Feature | Default | Provides |
| --- | --- | --- |
| `http` | yes | `WebhookReceiver` and its builder over `http::Request`, `Envelope::from_signed_headers` and `HeaderView` from an `http::HeaderMap`, `ResponseStatus` into `http::StatusCode` |
| `tower` | no | `tower_service::Service` for `WebhookReceiver`, so it mounts with `post_service` |
| `octocrab` | no | `FromEnvelope` for octocrab's decoded `WebhookEvent`, `Payload` for its per-kind payload structs, `Envelope::decode_event`. Makes octocrab's pre-1.0 types part of this crate's public API |
| `tracing` | no | verify, receive and dispatch spans, with nothing secret-derived in them |

The core (envelope, verification, both handler flavours, the dispatcher)
depends on none of them and builds for `wasm32-unknown-unknown`.

## Handlers

A handler is an `async fn` that takes what it handles and returns
`Result<(), E>`; the receiver and the dispatcher accept the function itself,
as the dispatcher above does with `audit` and `label`. Two flavours, named by
what they receive:

- A **webhook handler** receives the verified `Envelope`: the `EventMeta`
  (delivery ID, kind, action, installation ID, repository, sender) and the
  exact payload bytes. The receiver accepts one, and so do the dispatcher's
  `always` and `fallback` tiers. `audit` is one.
- An **event handler** receives the `EventMeta` and the envelope decoded as
  some `P: FromEnvelope`. `label` is one, over the `IssueOpened` view;
  `impl_payload!` declared which kind the view decodes, so
  `on_payload_action` needed no kind and could not be given the wrong one.
  `()` is an input too, for a handler routed by kind and action that
  decodes nothing; octocrab's `WebhookEvent` is one with the `octocrab`
  feature; and a view over fields several kinds share implements
  `FromEnvelope` itself and is registered under those kinds with `on`.

A handler with dependencies is a struct implementing the trait, the
dependencies its fields, borrowed through `&self` on every delivery.
`WebhookHandler` has the same shape with `handle(&self, envelope: Envelope)`:

```rust
use octoevents::{EventHandler, EventKind, EventMeta};

#[derive(serde::Deserialize)]
struct IssueOpened { issue: Issue }
#[derive(serde::Deserialize)]
struct Issue { number: u64 }
octoevents::impl_payload!(IssueOpened => EventKind::Issues);

struct Labeler {
    label: String, // stands in for a GitHub API client
}

impl EventHandler<IssueOpened> for Labeler {
    type Error = std::io::Error;

    async fn handle(&self, meta: EventMeta, payload: IssueOpened) -> Result<(), Self::Error> {
        println!("{}: label #{} {}", meta.delivery_id, payload.issue.number, self.label);
        Ok(())
    }
}
```

A struct keeps its own error type; the dispatcher converts it into the
application error through `From`, so `Labeler` registers on a
`Dispatcher<AppError>` once `AppError: From<std::io::Error>`. `Arc<H>` is a
handler when `H` is, for sharing one struct between a route and a test that
reads its state.

Closures work too, with two annotations the `async fn` form does not need:
name the type of each parameter the body uses (`|envelope: Envelope|`,
`|meta: EventMeta, issue: IssueOpened|`) and state the error type
(`Ok::<_, AppError>(())`). Registration is bound on the handler trait, not on
`Fn`, so rustc reads neither off the call.

## Routing

A `Dispatcher` is itself a webhook handler: the receiver hands it every
verified envelope, and it runs three tiers in order. `always` runs first,
for every delivery, and receives the envelope. The routed handlers run next:
those registered for the delivery's kind and action, then those for the kind.
`fallback` runs only when no routed handler matched, and receives the
envelope as `always` does. Within a tier, handlers run in registration order,
and the first error fails the delivery.

```rust,ignore
Dispatcher::<AppError>::builder()
    .always(audit)                                             // every delivery, first
    .on_payload(notify)                                        // kind from the payload type, every action
    .on_payload_action([Action::Opened], label)                // kind from the payload type, these actions
    .on((EventKind::Installation, Action::Deleted), revoke)    // over `()`: meta only, nothing decoded
    .on([EventKind::Issues, EventKind::IssueComment], metrics) // over a consumer `FromEnvelope` view
    .on(EventKind::PullRequest, triage)                        // over octocrab's `WebhookEvent`
    .fallback(reject)                                          // only if nothing matched
    .build()
```

`on_payload` and `on_payload_action` take the kind from the payload type;
`on` takes a matcher (a kind, several kinds, a kind with actions, or
kind/action pairs) and an event handler over any `FromEnvelope`. A routed
handler decodes its input only when its route matched, and `always` and
`fallback` decode nothing, so both run for a payload no handler can decode.
Unmatched deliveries succeed unless a fallback fails them.

A failure is a `DispatchError`: the application error wrapped with the tier,
the delivery's ID, kind and action, and the source location of the
registration that put the failing handler there. `dispatch` also reports an
`Outcome`, matched or unmatched with the kind known or unknown to the route
table, for a webhook handler wrapping the dispatcher to act on; see
[Delivery semantics](#delivery-semantics).

## Testing without GitHub

A handler is tested through `dispatch` with an envelope built by hand: an
`EventMeta` for the delivery and the payload bytes as a literal. Nothing is
signed, because nothing is verified on this path. In the same file as the
program above:

```rust,ignore
use octoevents::{Bytes, Match};

#[tokio::test]
async fn labels_an_opened_issue() {
    let dispatcher = Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], label)
        .build();

    let mut meta = EventMeta::new("delivery-1", EventKind::Issues);
    meta.action = Some(Action::Opened);
    let envelope = Envelope {
        meta,
        raw: Bytes::from_static(br#"{"action":"opened","issue":{"number":7,"title":"Add tests"}}"#),
    };

    let outcome = dispatcher.dispatch(envelope).await;

    assert_eq!(outcome.matched, Match::Matched);
    outcome.result.unwrap();
}
```

The receiver is tested with a signed synthetic request. GitHub's signature is
`sha256=` followed by the lowercase hex HMAC-SHA256 of the body under the
secret, and a request needs four headers: the signature, the delivery ID,
the event name and the content type, whose names `octoevents::header` spells.
With `hmac` and `sha2` as dev-dependencies:

```rust,ignore
use hmac::{Hmac, KeyInit as _, Mac as _};
use octoevents::header;
use sha2::Sha256;

/// What GitHub puts in `X-Hub-Signature-256`.
fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let hex: String = mac.finalize().into_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256={hex}")
}

#[tokio::test]
async fn accepts_a_signed_delivery() {
    let dispatcher = Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], label)
        .build();
    let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new("test-secret")))
        .build(dispatcher);

    let body = br#"{"action":"opened","issue":{"number":7,"title":"Add tests"}}"#;
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/webhook")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::DELIVERY_ID, "delivery-1")
        .header(header::EVENT_NAME, "issues")
        .header(header::SIGNATURE, sign("test-secret", body))
        .body(axum::body::Body::from(body.as_slice()))
        .unwrap();

    let response = webhook.receive(request).await;

    assert_eq!(response.status(), 204);
}
```

Change the secret on either side and the same request is answered 401.

## One event, one webhook handler

A receiver for one kind and nothing else needs no dispatcher. A webhook
handler decodes its own view with `decode_payload`, which refuses a delivery
of any other kind at the kind rather than at a missing field, so a webhook
subscribed to the wrong events fails loudly. Without the `tower` feature, the
receiver mounts on axum as a plain handler calling `receive`:

```rust,no_run
use axum::{Router, extract::Request, routing::post};
use octoevents::{
    Action, DecodeError, Envelope, EventKind, Secret, Verifier, WebhookReceiverBuilder,
};

#[derive(serde::Deserialize)]
struct ReleasePublished { release: Release, repository: Repository }
#[derive(serde::Deserialize)]
struct Release { tag_name: String }
#[derive(serde::Deserialize)]
struct Repository { full_name: String }
octoevents::impl_payload!(ReleasePublished => EventKind::Release);

async fn announce(envelope: Envelope) -> Result<(), DecodeError> {
    if envelope.meta.action != Some(Action::Published) {
        return Ok(());
    }
    let payload = envelope.decode_payload::<ReleasePublished>()?;
    println!("{} released {}", payload.repository.full_name, payload.release.tag_name);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;
    let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new(secret))).build(announce);

    let app = Router::new().route("/webhook", post(move |request: Request| {
        let webhook = webhook.clone();
        async move { webhook.receive(request).await }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

The alternative is a dispatcher with one `on_payload` route, which answers a
delivery of any other kind with success rather than failure.

## Without the `http` feature

The core is sans-I/O. A transport that has no `http::Request` (a serverless
runtime handing over a header map and a body string, say) builds a
`HeaderView` from the values under the names in `octoevents::header`, calls
`Envelope::from_signed` with the verifier and the body as `Bytes`, and
answers with `ResponseStatus`: `for_receive_error` for a failure there,
`NoContent` once the handler succeeded, `InternalServerError` when it failed.
The docs of `Envelope::from_signed` list the three things the receiver does
that this path does not: refusing an unsigned request before reading the
body, bounding the body, and short-circuiting `ping`. `Envelope` serializes
with serde for forwarding, bytes in base64; its docs show the document. The
`worker` example runs the receiver itself, through `receive`, on Cloudflare
Workers.

## Delivery semantics

GitHub signs no timestamp, so the crate provides no replay protection: treat
`EventMeta::delivery_id` as an idempotency key.

GitHub does not retry a failed delivery on its own, and it abandons a request
after 10 seconds (30 on GitHub Enterprise Server). Persist or forward an
envelope before returning and process it afterwards. With a dispatcher, that
policy lives in a webhook handler wrapping `dispatch`: it stores the envelope,
bytes included, before anything is routed; answers a redelivery of a stored
delivery ID with success without routing it; and reads the `Outcome` to
dead-letter a kind the route table does not know while tolerating an action
GitHub added to a kind it does. A tier can continue or fail but never skip,
which is why that wrapper, and not `always`, is the place; the `dispatcher`
example shows it. A forwarder that never needs to skip fits `always`.

The receiver answers a failed delivery with a bare 500 and never reads the
error: the response is GitHub's delivery record, not a log. The `on_error`
observer receives the `EventMeta` and the handler's error before the 500 is
answered, with no `Error`, `Display` or `Debug` bound on the error type, so a
boxed `dyn Error` is as observable as a named enum. It runs only when a
handler ran and failed: a request the receiver refused is a status code, and
a short-circuited `ping` reaches no handler.

## Real deliveries

The fastest way to see real deliveries is
[`gh webhook forward`](https://docs.github.com/en/webhooks/testing-and-troubleshooting-webhooks/using-the-github-cli-to-forward-webhooks-for-testing),
which creates a temporary webhook on a repository you administer and forwards
its deliveries (exact body bytes and headers, signature included) to
localhost. In one terminal, start a receiver, here the bundled Axum example:

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
the same delivery is refused with 401 instead. The webhook `gh` creates
delivers JSON, which is the content type this crate requires. If forwarding
fails with "you do not have access to this feature", the usual cause is a
token that cannot create webhooks on the repository; for a fine-grained
personal access token, grant the "Webhooks" repository permission (read and
write).

The `dispatcher` example is the production shape behind a receiver: a
persisting, deduplicating, dead-lettering webhook handler wrapping a
dispatcher that routes octocrab's payloads. Run it the same way and forward
`pull_request` events:

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
