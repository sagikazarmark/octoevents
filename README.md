# octoevents

[![ci](https://img.shields.io/github/actions/workflow/status/sagikazarmark/octoevents/dagger.yaml?style=flat-square&label=ci)](https://github.com/sagikazarmark/octoevents/actions/workflows/dagger.yaml)
[![openssf scorecard](https://api.securityscorecards.dev/projects/github.com/sagikazarmark/octoevents/badge?style=flat-square)](https://securityscorecards.dev/viewer/?uri=github.com/sagikazarmark/octoevents)
[![crates.io](https://img.shields.io/crates/v/octoevents?style=flat-square)](https://crates.io/crates/octoevents)
[![docs.rs](https://img.shields.io/docsrs/octoevents?style=flat-square)](https://docs.rs/octoevents)

**Receive and verify GitHub webhook events in Rust.**

A receiver that prints every verified delivery and says why one failed:

```rust
use octoevents::{DispatchError, Dispatcher, Envelope, Secret, Verifier, WebhookReceiverBuilder};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn print(envelope: Envelope) -> Result<(), BoxError> {
    println!("{} {}", envelope.meta.delivery_id, envelope.meta.kind);
    Ok(())
}

let dispatcher = Dispatcher::<BoxError>::builder().always(print).build();
let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new("development-secret")))
    .on_error(|_, error: &DispatchError<BoxError>| {
        eprintln!("{error}: {}", error.source);
        let mut cause = error.source.source();
        while let Some(error) = cause {
            eprintln!("  caused by: {error}");
            cause = error.source();
        }
    })
    .build(dispatcher);
```

`print` runs for every delivery whose signature verifies; a request that
does not verify is refused before it runs. `webhook` mounts on axum with
`post_service`, as the complete program below does.

The application error is a boxed `dyn Error`, so no error enum is written and
`?` converts any error inside a handler. The observer prints where the
delivery failed, then the handler's error, then each cause beneath it, down
to the field a payload was missing. Its walk starts at `error.source.source()`
rather than `error.source()` because a boxed `dyn Error` is not itself an
`Error`; the complete program's observer, over an error that is one, shows
the difference below.

**Coming from Probot?** The registrations map one to one. The rule that does
not: handlers run one at a time and the first error fails the delivery, where
Probot runs every matching handler and aggregates.

| Probot | octoevents |
| --- | --- |
| `app.on('issues.opened', h)` | `impl_payload!(IssueOpened => EventKind::Issues)` on a serde view of the payload, then `on_payload_action([Action::Opened], h)`, or `on((EventKind::Issues, Action::Opened), h)` with the kind spelled at the registration; `on` also takes a handler over the envelope or the meta. There is no string route form |
| `app.on('issues', h)` | `on_payload(h)` after the same `impl_payload!`, or `on(EventKind::Issues, h)` |
| `app.onAny(h)` | `always(h)`: runs first, for every delivery, over the envelope; its error fails the delivery; sees `ping` only when the receiver is built with `handle_ping(true)` |
| `app.onError(h)` | `on_error(h)` on the receiver builder |
| `app.receive(event)` | `dispatcher.dispatch(envelope)` with an envelope from `Envelope::new`; see [Testing without GitHub](#testing-without-github) |

A complete receiver that labels every opened issue, audits every delivery,
and reports every failure with its causes:

```rust,no_run
use std::error::Error as _;

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
async fn label(issue: IssueOpened) -> Result<(), AppError> {
    println!("label #{} '{}'", issue.issue.number, issue.issue.title);
    Ok(())
}

/// Runs for every delivery the dispatcher is handed, bytes included.
async fn audit(envelope: Envelope) -> Result<(), AppError> {
    println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw_payload.len());
    Ok(())
}

/// Runs when a handler failed, before the 500 is answered: prints where the
/// delivery failed, then why, one cause per line.
fn report(_: &EventMeta, error: &DispatchError<AppError>) {
    eprintln!("{error}");
    let mut cause = error.source();
    while let Some(error) = cause {
        eprintln!("  caused by: {error}");
        cause = error.source();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;

    let dispatcher = Dispatcher::<AppError>::builder()
        .always(audit)
        .on_payload_action([Action::Opened], label)
        .build();

    let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new(secret)))
        .on_error(report)
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
as the view it asked for. The `on_error` observer is where a failure's cause
becomes visible; without it a failed delivery is a bare 500 (and, with the
`tracing` feature, one ERROR event naming the delivery). `report` prints
where (the tier, the failing handler's name and the line that registered it)
and why (the handler's own error, then each cause beneath it); for a payload
without the `title` the view names, that is:

```text
delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the route tier at the handler `app::label` registered at src/main.rs:60:10
  caused by: payload could not be decoded
  caused by: missing field `title` at line 1 column 39
```

With the `tracing` feature, `.trace_errors()` on the receiver builder is
`report` as fields on that one ERROR event, source chain included.

`report` walks the chain from `error.source()`; the hello world's observer
walked it from `error.source.source()`. `AppError` is an `Error`, so
`DispatchError<AppError>` is one too, with the application error as its
`source()`. `Box<dyn Error + Send + Sync>` is not itself an `Error`, so
neither is `DispatchError` over it, and there is no `source()` to call on the
error the observer receives: the handler's error is the `source` field, an
`Error` in its own right, and the chain continues from there. For the same
reason a receiver over the boxed error calls `.trace_boxed_errors()` where
this one calls `.trace_errors()`: it takes an error behind a pointer, or a
`DispatchError` over one, and puts the same text and sources on the event.

## Features

| Feature | Default | Provides |
| --- | --- | --- |
| `http` | yes | `WebhookReceiver` and its builder over `http::Request`, `HeaderView` from an `http::HeaderMap`, `ResponseStatus` into `http::StatusCode` |
| `tower` | no | `tower_service::Service` for `WebhookReceiver`, so it mounts with `post_service` as above. Without it, the receiver mounts on axum as a plain handler calling `receive`; [One event, one handler](#one-event-one-handler) shows the wiring |
| `octocrab` | no | `FromEnvelope` for octocrab's decoded `WebhookEvent`, `Payload` for its per-kind payload structs, `Envelope::decode_event`. octocrab goes in your own `[dependencies]` too, to name its types; this crate re-exports none of them. The trade-off is whole-model decode: a field GitHub changes fails the delivery with 500, where a view fails only on the fields it names, and a test fixture is a complete payload, since the structs decode no fragment. Some structs leave their main object untyped, as `serde_json::Value` (`check_suite`, `workflow_run`, `workflow_job`, `team`). The per-kind payload structs omit the top-level `installation`, `sender`, `repository` and `organization` objects, which `WebhookEvent` carries and `EventMeta` summarizes. Makes octocrab's pre-1.0 types part of this crate's public API |
| `tracing` | no | receive and dispatch spans at INFO and a verify span at DEBUG, and one ERROR event per failed delivery, which `trace_errors` (for an `Error`) or `trace_boxed_errors` (for a boxed one, or a `DispatchError` over it) on the receiver builder extends with the error's text and sources; nothing secret-derived in any of them. The contract is on the crate's front page |

The core (envelope, verification, the handler trait and its inputs, the
dispatcher) depends on none of them and builds for `wasm32-unknown-unknown`.

## Handlers

A handler is an `async fn` that takes one input and returns `Result<(), E>`;
the receiver and the dispatcher accept the function itself, as the dispatcher
above does with `audit` and `label`. One trait, `Handler<I>`, and the input
type `I` says what the handler receives and what is decoded for it:

- `Envelope`: the `EventMeta` (delivery ID, kind, action, installation ID,
  repository, organization, sender, target type and target ID) and the exact
  payload bytes, nothing decoded. What the receiver and the dispatcher's
  `always` and `fallback` tiers take; `audit` is one.
  `async fn audit(envelope: Envelope)`
- `EventMeta`: the meta alone, for a handler routed by kind and action that
  reads no payload. `async fn revoke(meta: EventMeta)`
- A `Payload` view `P`: the payload decoded as `P`, kind checked. The kind
  comes from the type: `impl_payload!` declared which kind `IssueOpened`
  decodes, so `on_payload_action` needed none and could not be given the
  wrong one. `label` is one. `async fn label(issue: IssueOpened)`
- `Event<P>`: the meta beside the payload, in one parameter. Meta and payload
  together is `Event<P>`, not two parameters; destructure it or read
  `event.meta` and `event.payload`.
  `async fn notify(Event { meta, payload }: Event<IssueOpened>)`

A view over fields several kinds share (the sender, say) implements
`FromEnvelope` itself with the kind-free `Envelope::decode` and is registered
under those kinds with `on`. With the `octocrab` feature, octocrab's
`WebhookEvent` is an input too, on its own or inside `Event`, and its per-kind
payload structs are payloads.

A handler with dependencies is a struct implementing the trait, the
dependencies its fields, borrowed through `&self` on every delivery. A struct
keeps its own error type, and the dispatcher converts it into the application
error through `From`, so the `Dispatcher<AppError>` that `Labeler` registers
on needs `AppError: From<std::io::Error>`, the `Io` variant here:

```rust
use octoevents::{DecodeError, Dispatcher, Event, EventKind, Handler};

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(serde::Deserialize)]
struct IssueOpened { issue: Issue }
#[derive(serde::Deserialize)]
struct Issue { number: u64 }
octoevents::impl_payload!(IssueOpened => EventKind::Issues);

struct Labeler {
    label: String, // stands in for a GitHub API client
}

impl Handler<Event<IssueOpened>> for Labeler {
    type Error = std::io::Error;

    async fn handle(&self, Event { meta, payload }: Event<IssueOpened>) -> Result<(), Self::Error> {
        println!("{}: label #{} {}", meta.delivery_id, payload.issue.number, self.label);
        Ok(())
    }
}

let dispatcher = Dispatcher::<AppError>::builder()
    .on_payload(Labeler { label: "triage".into() })
    .build();
```

Without the `Io` variant the impl is fine and the registration is not: rustc
reports E0277, "the trait bound `AppError: From<std::io::Error>` is not
satisfied", at the `on_payload` call.

Closures work too, with two annotations the `async fn` form does not need:
name the parameter's type (`|envelope: Envelope|`, `|issue: IssueOpened|`,
`|Event { meta, payload }: Event<IssueOpened>|`) and state the error type
(`Ok::<_, AppError>(())`). Registration is bound on the handler trait, not on
`Fn`, so rustc reads neither off the call.

## Routing

A `Dispatcher` is itself a handler over the envelope: the receiver hands it
every verified envelope, and it runs three tiers in order. `always` runs
first, for every delivery, and receives the envelope. The routed handlers run
next: those registered for the delivery's kind and action, then those for the
kind. `fallback` runs only when no routed handler matched, and receives the
envelope as `always` does. Within a tier, handlers run in registration order,
and the first error fails the delivery.

```rust,ignore
Dispatcher::<AppError>::builder()
    .always(audit)                                             // every delivery, first
    .on_payload(notify)                                        // kind from the payload type, every action
    .on_payload_action([Action::Opened], label)                // kind from the payload type, these actions
    .on((EventKind::Issues, Action::Opened), welcome)          // over a payload view too, kind spelled at the registration
    .on((EventKind::Installation, Action::Deleted), revoke)    // over `EventMeta`: meta only, nothing decoded
    .on(EventKind::Push, forward)                              // over `Envelope`: bytes included, for one kind
    .on([EventKind::Issues, EventKind::IssueComment], metrics) // over a consumer `FromEnvelope` view
    .on(EventKind::PullRequest, triage)                        // over octocrab's `WebhookEvent`
    .build()
```

`on_payload` and `on_payload_action` take the kind from the payload type, and
accept a handler over the payload `P` or over `Event<P>`; `on` takes a matcher
(a kind, several kinds, a kind with actions, or kind/action pairs) and a
handler over any `FromEnvelope` input, a payload view included.
`on((EventKind::Issues, Action::Opened), welcome)` is the Probot spelling of
`on_payload_action([Action::Opened], welcome)`, with the kind said twice, on
the type and at the registration. The two are compared at dispatch, not at
compile time: a matcher that disagrees with the view's kind fails every
delivery it routes with `DecodeError::KindMismatch`, reported at that
registration. A routed handler decodes its input only when its route matched,
and `always` and `fallback` decode nothing, so both run for a payload no
handler can decode. Unmatched deliveries succeed unless a fallback fails
them; a strict fallback that rejects every kind nothing routes is one
registration, shown in
[its rustdoc](https://docs.rs/octoevents/latest/octoevents/struct.DispatcherBuilder.html#method.fallback).

A failure is a `DispatchError`: the application error wrapped with the tier,
the delivery's ID, kind and action, the failing handler's name, and the
source location of the registration that put it there. `dispatch` also
reports an `Outcome`, matched or unmatched with the kind known or unknown to
the route table, beside the result.

## Testing without GitHub

A handler is tested through `dispatch` with an envelope built by
`Envelope::new`: the delivery ID, the kind, and the payload bytes as a
literal. Nothing is signed, because nothing is verified on this path; what
the constructor does do is read the action, installation ID, repository,
organization and sender out of the bytes, the way the receiver does, so the
meta a handler over `Event<P>` sees is what the payload says rather than what
the test remembered to assign. (`EventMeta::new` on its own reads no bytes;
it is for a handler over the meta alone.) In the same file as the program
above:

```rust,ignore
use std::sync::{Arc, Mutex};

use octoevents::{Event, Match};

#[tokio::test]
async fn labels_an_opened_issue() {
    let dispatcher = Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], label)
        .build();

    let envelope = Envelope::new(
        "delivery-1",
        EventKind::Issues,
        br#"{"action":"opened","issue":{"number":7,"title":"Add tests"}}"#,
    );

    let outcome = dispatcher.dispatch(envelope).await;

    assert_eq!(outcome.matched, Match::Matched);
    outcome.result.unwrap();
}

/// The meta a handler over `Event<P>` receives is what the payload carries:
/// the action routed it, and the installation ID is beside the decoded view.
#[tokio::test]
async fn the_installation_reaches_the_handler_from_the_payload() {
    let seen = Arc::new(Mutex::new(None));
    let dispatcher = Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], {
            let seen = Arc::clone(&seen);
            move |Event { meta, payload }: Event<IssueOpened>| {
                let seen = Arc::clone(&seen);
                async move {
                    *seen.lock().unwrap() = Some((meta.installation_id, payload.issue.number));
                    Ok::<_, AppError>(())
                }
            }
        })
        .build();

    let envelope = Envelope::new(
        "delivery-2",
        EventKind::Issues,
        br#"{"action":"opened","installation":{"id":42},"issue":{"number":7,"title":"Add tests"}}"#,
    );

    dispatcher.dispatch(envelope).await.result.unwrap();
    assert_eq!(*seen.lock().unwrap(), Some((Some(42), 7)));
}
```

`Envelope` cannot be built as a struct literal outside the crate, so a meta
cannot be paired with a payload that says something else. A field the payload
does not carry, the target type and ID from GitHub's headers, is assigned
afterwards on a `mut` binding if the handler reads it.

`Match::Matched` has two unmatched siblings. A payload without an `action`
against this dispatcher is `Match::UnmatchedAction`: the kind is registered,
the action is not, and the result is `Ok` because nothing ran. A `push`
envelope is `Match::UnmatchedKind`.

The receiver is tested with a signed synthetic request. GitHub's signature is
`sha256=` followed by the lowercase hex HMAC-SHA256 of the body under the
secret, and a request needs four headers: the signature, the delivery ID,
the event name and the content type, whose names `octoevents::header` spells.
The body is a `String`: `receive` takes any `http_body::Body` over `Bytes`,
and `String` is one, so the test needs no axum. With `hmac = "0.13"`,
`sha2 = "0.11"` and `http = "1"` as dev-dependencies:

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

    let body = r#"{"action":"opened","issue":{"number":7,"title":"Add tests"}}"#;
    let request = http::Request::builder()
        .method("POST")
        .uri("/webhook")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::DELIVERY_ID, "delivery-1")
        .header(header::EVENT_NAME, "issues")
        .header(header::SIGNATURE, sign("test-secret", body.as_bytes()))
        .body(body.to_string())
        .unwrap();

    let response = webhook.receive(request).await;

    assert_eq!(response.status(), 204);
}
```

Change the secret on either side and the same request is answered 401.

## One event, one handler

A receiver for one kind and nothing else needs no dispatcher. A handler over
the envelope decodes its own view with `decode_payload`, which refuses a
delivery of any other kind at the kind rather than at a missing field, so a
webhook subscribed to the wrong events fails loudly. Without the `tower`
feature, the receiver mounts on axum as a plain handler calling `receive`:

```rust,no_run
use std::error::Error as _;

use axum::{Router, extract::Request, routing::post};
use octoevents::{
    Action, DecodeError, Envelope, EventKind, EventMeta, Secret, Verifier, WebhookReceiverBuilder,
};

#[derive(serde::Deserialize)]
struct ReleasePublished { release: Release }
#[derive(serde::Deserialize)]
struct Release { tag_name: String }
octoevents::impl_payload!(ReleasePublished => EventKind::Release);

async fn announce(envelope: Envelope) -> Result<(), DecodeError> {
    if envelope.meta.action != Some(Action::Published) {
        return Ok(());
    }
    // The repository is already on the meta; only the tag needs the payload.
    let repository = envelope.meta.repository.as_ref().map_or("?", |r| r.full_name.as_str());
    let payload = envelope.decode_payload::<ReleasePublished>()?;
    println!("{repository} released {}", payload.release.tag_name);
    Ok(())
}

/// The handler's error is the whole story here: a delivery of another kind,
/// or a payload without the field the view names, one cause per line.
fn report(meta: &EventMeta, error: &DecodeError) {
    eprintln!("delivery {} failed: {error}", meta.delivery_id);
    let mut cause = error.source();
    while let Some(error) = cause {
        eprintln!("  caused by: {error}");
        cause = error.source();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;
    let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new(secret)))
        .on_error(report)
        .build(announce);

    let app = Router::new().route("/webhook", post(move |request: Request| {
        let webhook = webhook.clone();
        async move { webhook.receive(request).await }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

Without a dispatcher there is no tier or registration site to name, so
`report` names the delivery itself; for a release payload without
`tag_name`, it prints `payload could not be decoded` and then
`` missing field `tag_name` ``. The alternative is a dispatcher with one
`on_payload` route, which answers a delivery of any other kind with success
rather than failure.

## Without the `http` feature

The core is sans-I/O. A transport that has no `http::Request` (a serverless
runtime handing over a header map and a body string, say) builds a
`HeaderView` with `HeaderView::from_lookup`, which asks the map for each
header by the names in `octoevents::header`, calls `Envelope::from_signed`
with the verifier and the body as `Bytes`, and answers with `ResponseStatus`:
`for_receive_error` for a failure there, `NoContent` once the handler
succeeded, `InternalServerError` when it failed. The header names are
lowercase and the lookup compares nothing itself, so matching the case of the
map's keys is the transport's concern: a map that kept GitHub's
`X-GitHub-Delivery` casing is lowercased first. `Dispatcher::dispatch` is a
plain `async fn` with no runtime of its own, so a transport awaits it on
whatever executor it has. The docs of `Envelope::from_signed` show, as code
to copy, the three things the receiver does that this path does not: refusing
an unsigned request before reading the body, bounding the body, and
short-circuiting `ping`. `Envelope` serializes with serde for forwarding,
bytes in base64; its docs show the document. The `worker` example runs the
receiver itself, through `receive`, on Cloudflare Workers.

## Delivery semantics

GitHub signs no timestamp, so the crate provides no replay protection: treat
`EventMeta::delivery_id` as an idempotency key.

GitHub does not retry a failed delivery on its own, and it abandons a request
after 10 seconds (30 on GitHub Enterprise Server). Persist or forward an
envelope before returning and process it afterwards. With a dispatcher,
persisting, answering a redelivery of a stored delivery ID with success, and
dead-lettering an unmatched delivery all live in a handler wrapping
`dispatch`, [the policy seam][policy-seam], which the `dispatcher` example
shows. Once a wrapper persists and deduplicates, a redelivery requested for a
failed delivery (red in the webhook's recent deliveries) arrives under the
stored delivery ID and is skipped as a duplicate: the 500 is then a signal to
the operator, not a request to GitHub, and the delivery is recovered from the
store.

[policy-seam]: https://docs.rs/octoevents/latest/octoevents/struct.Dispatcher.html#the-policy-seam

The receiver answers a failed delivery with a bare 500 and never reads the
error: the response is GitHub's delivery record, not a log. The `on_error`
observer receives the `EventMeta` and the handler's error before the 500 is
answered, with no `Error`, `Display` or `Debug` bound on the error type, so a
boxed `dyn Error` is as observable as a named enum. It runs only when a
handler ran and failed: a request the receiver refused is a status code, and
a short-circuited `ping` reaches no handler.

GitHub sends a `ping` when a webhook is created. The receiver answers a
verified one with 204 before any handler runs, so a dispatcher never routes
it and `always` never sees it; `handle_ping(true)` on the receiver builder
passes it through instead, for an `always` handler that records every
delivery to record that one too. An unsigned `ping` is 401 either way.

GitHub holds one secret per webhook, so a rotation window is the verifier's
to open: `Verifier::new(current).also(next)` verifies against either while
the secret is changed in the webhook's settings, and `Verifier::new(next)`
alone once the deliveries signed with the old one have drained. Every secret
is evaluated on every request, a match included, and an empty secret panics
at construction rather than verifying against a guessable key.

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
persisting, deduplicating, dead-lettering handler wrapping a dispatcher that
routes octocrab's payloads. Run it the same way and forward
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
