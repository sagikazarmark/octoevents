# octoevents

[![ci](https://img.shields.io/github/actions/workflow/status/sagikazarmark/octoevents/dagger.yaml?style=flat-square&label=ci)](https://github.com/sagikazarmark/octoevents/actions/workflows/dagger.yaml)
[![openssf scorecard](https://api.securityscorecards.dev/projects/github.com/sagikazarmark/octoevents/badge?style=flat-square)](https://securityscorecards.dev/viewer/?uri=github.com/sagikazarmark/octoevents)
[![crates.io](https://img.shields.io/crates/v/octoevents?style=flat-square)](https://crates.io/crates/octoevents)
[![docs.rs](https://img.shields.io/docsrs/octoevents?style=flat-square)](https://docs.rs/octoevents)

**Receive and verify GitHub webhook events in Rust.**

A receiver turns an untrusted HTTP request into a verified envelope: the
exact payload bytes and the delivery's routing metadata. A dispatcher routes
envelopes to your handlers by event kind and action. The core is sans-I/O and
builds for `wasm32-unknown-unknown`; the receiver over `http` types mounts on
axum, Cloudflare Workers, or anything else that can hand over a request.

> **Coming from Probot?** The registrations map one to one. See
> [Migrating from Probot](#migrating-from-probot).

## Quickstart

A receiver that thanks the author of every opened issue:

```rust,no_run
use axum::{Router, routing::post_service};
use octoevents::{Action, Dispatcher, Envelope, EventKind, Verifier, WebhookReceiverBuilder, WebhookSecret};

// The error every handler returns. Any error converts into it with `?`.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Runs for `issues.opened`. The envelope is the verified delivery: its meta
/// (delivery ID, kind, action, repository, sender, ...) and the raw payload.
async fn thank(envelope: Envelope) -> Result<(), BoxError> {
    let sender = envelope.meta.sender.map(|s| s.login).unwrap_or_default();
    println!("Thank you for your contribution, @{sender}! :)");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;

    // Routes each verified envelope by kind and action.
    let dispatcher = Dispatcher::<BoxError>::builder()
        .on((EventKind::Issues, Action::Opened), thank)
        .build();

    // Verifies `X-Hub-Signature-256` against the body with this secret.
    // A receiver cannot be built without one.
    let verifier = Verifier::new(WebhookSecret::new(secret));

    // Refuses what does not verify; hands everything else to the dispatcher.
    let webhook = WebhookReceiverBuilder::new(verifier).build(dispatcher);

    // The receiver owns no paths or methods: mount it on your router.
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

`serde` is for the payload views under [Handlers](#handlers) and `thiserror`
for the error enum under [Error handling](#error-handling); the quickstart
itself reads the envelope's meta, returns a boxed error, and needs neither.

Every request goes through three steps:

1. **Verify.** `X-Hub-Signature-256` is checked against the exact body bytes
   with the secret. A request whose signature header is absent or malformed
   is refused before its body is read.
2. **Route.** The dispatcher matches the delivery's kind and action: `thank`
   runs for `issues.opened`; any other delivery succeeds with nothing run.
3. **Answer.** GitHub gets a bare status and no body:

| Status | When |
| --- | --- |
| 204 | The handler succeeded, or the delivery was a `ping` (answered before any handler) |
| 500 | The handler failed; see [Error handling](#error-handling) |
| 401 | The signature is missing or does not match |
| 400 | The signature is malformed, a header is missing, the content type is not JSON, or the body could not be read |
| 413 | The body is over the limit (25 MiB by default) |

A 500 is a bare status, and out of the box nothing else says a handler
failed: an observer registered with `on_error` is where the error reaches
your code, as [Error handling](#error-handling) shows, and with the
`tracing` feature the receiver also emits one event at ERROR per failed
delivery, as [Tracing](#tracing) describes.

`thank` took the whole envelope; a handler can take the decoded payload
instead, and a dispatcher can run several handlers in tiers. The next
sections cover both.

## Try it

[`gh webhook forward`](https://docs.github.com/en/webhooks/testing-and-troubleshooting-webhooks/using-the-github-cli-to-forward-webhooks-for-testing)
creates a temporary webhook on a repository you administer and forwards its
deliveries (exact bytes and headers, signature included) to localhost.

In one terminal, start a receiver: the quickstart, or the bundled example:

```console
GITHUB_WEBHOOK_SECRET=development-secret cargo run --example axum --features tower
```

In another, forward `issues` events from your repository, signed with the
same secret:

```console
gh extension install cli/gh-webhook
gh webhook forward --repo=<owner>/<repo> --events=issues \
  --url=http://127.0.0.1:3000/webhook --secret=development-secret
```

Open an issue in the repository and the receiver prints its line. Change the
secret on either side and the same delivery is refused with 401.

If forwarding fails with "you do not have access to this feature", the token
usually cannot create webhooks on the repository; a fine-grained personal
access token needs the "Webhooks" repository permission (read and write).

The `dispatcher` example is the production shape: a persisting, deduplicating,
dead-lettering handler wrapping a dispatcher that routes octocrab's payloads.
Run it the same way and forward `pull_request` events:

```console
GITHUB_WEBHOOK_SECRET=development-secret cargo run --example dispatcher --features tower,octocrab
```

## Handlers

A handler is an `async fn` that takes one input and returns `Result<(), E>`.
There is one trait, `Handler<I>`, and the input type `I` says what the handler
receives and what is decoded for it:

| Input | The handler receives | Decoded | Registered with |
| --- | --- | --- | --- |
| `Envelope` | The meta and the exact payload bytes | Nothing | The receiver, `always`, `fallback`, `on` |
| `EventMeta` | The meta alone | Nothing | `on` |
| `P: Payload` | The payload as `P` | `P`, kind checked | `on`, with the kind from `P` or spelled |
| `Event<P>` | The meta beside the payload as `P` | `P`, kind checked | `on`, with the kind from `P` or spelled |

A `Payload` is a serde view over the fields a handler reads, declaring the
event kind it decodes with `#[derive(Payload)]` and `#[payload(EventKind::..)]`.
It fails only on the fields it names, so a field GitHub adds elsewhere in the
document changes nothing.

```rust
use octoevents::{Action, Dispatcher, Envelope, Event, EventKind, EventMeta, Payload};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

// A view over an `issues` payload: only what the handlers read, and the kind
// it decodes from.
#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::Issues)]
struct IssueOpened {
    issue: Issue,
}

#[derive(serde::Deserialize)]
struct Issue {
    number: u64,
    title: String,
}

// Envelope: the meta and the raw bytes, nothing decoded.
async fn audit(envelope: Envelope) -> Result<(), BoxError> {
    let meta = &envelope.meta;
    println!("{} {} ({} bytes)", meta.delivery_id, meta.kind, envelope.raw_payload.len());
    Ok(())
}

// EventMeta: the meta only, for a handler that reads no payload.
async fn revoke(meta: EventMeta) -> Result<(), BoxError> {
    println!("revoke tokens for installation {:?}", meta.installation_id);
    Ok(())
}

// Payload: decoded as the view. The kind comes from the type.
async fn label(issue: IssueOpened) -> Result<(), BoxError> {
    println!("label #{} '{}'", issue.issue.number, issue.issue.title);
    Ok(())
}

// Event<P>: meta and payload together, as one parameter.
async fn notify(Event { meta, payload }: Event<IssueOpened>) -> Result<(), BoxError> {
    println!("{}: #{} opened", meta.delivery_id, payload.issue.number);
    Ok(())
}

let dispatcher = Dispatcher::<BoxError>::builder()
    .always(audit)                                          // every delivery
    .on((EventKind::Installation, Action::Deleted), revoke) // kind and action spelled here
    .on(Action::Opened, label)                              // kind from `IssueOpened`
    .on(Action::Opened, notify)
    .build();
```

Meta and payload together is one input, `Event<P>`, not two parameters.

### Struct handlers and closures

A handler with dependencies is a struct implementing `Handler<I>`; the
dependencies are its fields, borrowed through `&self` on every delivery. The
struct keeps its own error type, and the dispatcher converts it into the
application error through `From`:

```rust
use octoevents::{AnyAction, Dispatcher, Event, EventKind, Handler, Payload};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::Issues)]
struct IssueOpened { issue: Issue }
#[derive(serde::Deserialize)]
struct Issue { number: u64 }

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

let dispatcher = Dispatcher::<BoxError>::builder()
    .on(AnyAction, Labeler { label: "triage".into() })
    .build();
```

Closures work too, with two annotations the `async fn` form does not need:
the parameter's type and the error type, since registration is bound on the
handler trait rather than on `Fn`:

```rust,ignore
.always(|envelope: Envelope| async move {
    println!("{}", envelope.meta.delivery_id);
    Ok::<_, BoxError>(())
})
```

### octocrab payloads

With the `octocrab` feature, octocrab's `WebhookEvent` is an input (on its
own or inside `Event`) and its per-kind payload structs
(`PullRequestWebhookEventPayload`, ...) are payloads, so nothing needs to be
written for a handler over a whole event. The trade-off is whole-model decode:
the struct names far more of the payload than a handler reads, and an
incompatible change to any field it names (removed, renamed, retyped, or made
null) fails the delivery, where a view names the fields its handler reads and
fails only on those; a field GitHub adds fails neither. A test fixture for a
struct is a complete payload. Two gaps to know before choosing a struct for a
kind: the per-kind
structs omit the top-level `installation`, `sender`, `repository` and
`organization` objects (`EventMeta` summarizes them; `WebhookEvent` carries
them whole, so an `installation` handler that needs the account or the
permissions takes `Event<WebhookEvent>` or a view), and some leave their main
object as an untyped `serde_json::Value` (`check_suite`, `workflow_run`,
`workflow_job`, `team`). octocrab goes in your own `[dependencies]` to name
its types; the caveats are under
[Feature caveats](https://docs.rs/octoevents/latest/octoevents/#feature-caveats).

## Dispatcher

A `Dispatcher` is itself a handler over the envelope. Per delivery it runs
three tiers in order:

| Tier | Runs | Receives | Registered with |
| --- | --- | --- | --- |
| Always | First, for every delivery | `Envelope` | `always` |
| Route | The handlers matching the kind and action, then those matching the kind | Any input | `on` |
| Fallback | Only when no route matched | `Envelope` | `fallback` |

Within a tier, handlers run in registration order, and the first error fails
the delivery. There are no priorities and no propagation control.

### Routing

`on` takes a matcher and a handler over any input. A matcher that says its
kinds (a kind, several kinds, a kind with actions, or kind/action pairs)
routes any handler. For a handler over a payload type, the matcher may say
actions alone (an `Action`, an array of them, or `AnyAction` for every
action) and the kind comes from the type.

```rust
use octoevents::{Action, AnyAction, Dispatcher, Envelope, EventKind, EventMeta, Payload};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::Issues)]
struct IssueOpened { issue: Issue }
#[derive(serde::Deserialize)]
struct Issue { number: u64 }

async fn audit(envelope: Envelope) -> Result<(), BoxError> {
    println!("{} {}", envelope.meta.delivery_id, envelope.meta.kind);
    Ok(())
}
async fn notify(issue: IssueOpened) -> Result<(), BoxError> {
    println!("#{} changed", issue.issue.number);
    Ok(())
}
async fn label(issue: IssueOpened) -> Result<(), BoxError> {
    println!("label #{}", issue.issue.number);
    Ok(())
}
async fn revoke(meta: EventMeta) -> Result<(), BoxError> {
    println!("revoke tokens for installation {:?}", meta.installation_id);
    Ok(())
}
async fn forward(envelope: Envelope) -> Result<(), BoxError> {
    println!("forward {} bytes", envelope.raw_payload.len());
    Ok(())
}
async fn reject(envelope: Envelope) -> Result<(), BoxError> {
    Err(format!("unhandled event: {}", envelope.meta.kind).into())
}

let dispatcher = Dispatcher::<BoxError>::builder()
    // Always: first, for every delivery, bytes included.
    .always(audit)
    // Routes with the kind taken from the payload type...
    .on(AnyAction, notify)                                  // every `issues` action
    .on([Action::Opened, Action::Reopened], label)          // these actions only
    // ...or spelled at the registration, for a handler over any input.
    .on((EventKind::Installation, Action::Deleted), revoke) // meta only
    .on([EventKind::Push, EventKind::Create], forward)      // bytes, for these kinds
    // Fallback: only when no route matched. This one is strict.
    .fallback(reject)
    .build();
```

`on((EventKind::Issues, Action::Opened), label)` is the Probot spelling of
`on(Action::Opened, label)`, with the kind said twice. The two are compared
at dispatch, not at compile time: a matcher that disagrees with the view's
kind fails every delivery it routes with `DecodeError::KindMismatch`. Actions
alone under a handler whose input declares no kind (`EventMeta`, `Envelope`)
are refused at compile time, with a message that says to spell the kind.

A routed handler decodes its input only when its route matched, so a handler
registered for some actions decodes nothing for a delivery carrying another.
A view over fields several kinds share (the sender, say) implements
`FromEnvelope` itself with the kind-free `Envelope::decode` and registers
under those kinds with `on`; the
[`Dispatcher` docs](https://docs.rs/octoevents/latest/octoevents/struct.Dispatcher.html)
show one.

### Always and fallback

- `always` runs first and never counts as a match, so a strict fallback still
  rejects kinds nothing else handles. Its failure fails the delivery before
  routing begins.
- `fallback` runs only when no route matched. It cannot see why: a kind the
  route table never registered and an action GitHub added to a kind it did
  arrive alike. It is empty by default, so unmatched deliveries succeed.
- Neither decodes anything, so both run for a payload no routed handler can
  decode.
- A tier can continue or fail, never skip. A policy the tiers cannot express
  (answer a redelivery of a stored delivery ID with success, dead-letter an
  unmatched delivery) lives in a handler over the envelope that wraps
  `dispatch`, [the policy seam][policy-seam]; the `dispatcher` example shows
  one. GitHub abandons a request after 10 seconds and does not retry a failed
  delivery on its own, so that wrapper is also where an envelope is persisted
  before the response goes out.

[policy-seam]: https://docs.rs/octoevents/latest/octoevents/struct.Dispatcher.html#the-policy-seam

### Outcome

`dispatch` reports an `Outcome`: whether the delivery matched, and the result
of the handlers that ran. The two are independent: a matched delivery can
fail, and an unmatched one succeeds unless a fallback fails it.

| `outcome.matched` | Meaning |
| --- | --- |
| `Match::Matched` | At least one routed handler is registered for the kind, or the kind and action |
| `Match::UnmatchedAction` | The kind is registered, this action is not |
| `Match::UnmatchedKind` | The route table never registered the kind |

As the receiver's handler, the dispatcher keeps only the result; the outcome
is for a wrapper to read.

## Error handling

The dispatcher is `Dispatcher<E>` over one application error `E`. Two
conversions are required: `E: From<H::Error>` for every registered handler,
and `E: From<DecodeError>` for every handler registered with `on`, because
the dispatcher decodes a routed handler's input on its behalf and reports a
payload that does not fit through `E` (`always` and `fallback` decode
nothing and ask nothing of `E`). `Box<dyn Error + Send + Sync>` satisfies
both, which is why the quickstart needed no error type of its own. The named
alternative is an enum:

```rust
use std::error::Error as _;

use octoevents::{DecodeError, DispatchError, Dispatcher, EventMeta, Verifier, WebhookReceiverBuilder, WebhookSecret};

#[derive(Debug, thiserror::Error)]
enum AppError {
    // What the dispatcher reports for a payload that does not fit a view.
    #[error(transparent)]
    Decode(#[from] DecodeError),
    // What the handlers' own dependencies fail with.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Runs when a handler failed, before the 500 is answered: where, then why,
/// one cause per line.
fn report(_: &EventMeta, error: &DispatchError<AppError>) {
    eprintln!("{error}");
    let mut cause = error.source();
    while let Some(error) = cause {
        eprintln!("  caused by: {error}");
        cause = error.source();
    }
}

let dispatcher = Dispatcher::<AppError>::builder().build();

let webhook = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("development-secret")))
    .on_error(report)
    .build(dispatcher);
```

A failure is a `DispatchError`: the handler's error wrapped with the tier, the
delivery's ID, kind and action, the failing handler's name, and the source
location of the registration that put it there. Its text says *where*; its
source says *why*. For a payload without the `title` a view names, `report`
prints:

```text
delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the route tier at the handler `app::label` registered at src/main.rs:60:10
  caused by: payload could not be decoded
  caused by: missing field `title` at line 1 column 39
```

The response stays a bare 500 either way: it is GitHub's delivery record, not
a log. The `on_error` observer is synchronous, places no bound on the error
type, and runs whenever the receiver's handler fails: with a dispatcher, that
includes a payload that could not be decoded for a routed handler, as above.
It never runs for a refused request, which is a status code, or for a
short-circuited `ping`, which reaches no handler.

**Boxed errors.** `Box<dyn Error + Send + Sync>` is not itself an `Error`, so
neither is `DispatchError` over it, and there is no `source()` to call on what
the observer receives. The handler's error is the `source` field, and the
chain continues from there:

```rust,ignore
.on_error(|_, error: &DispatchError<BoxError>| {
    eprintln!("{error}: {}", error.source);
    let mut cause = error.source.source();
    while let Some(error) = cause {
        eprintln!("  caused by: {error}");
        cause = error.source();
    }
})
```

## Testing without GitHub

A handler is tested through `dispatch` with an envelope from `Envelope::new`:
the delivery ID, the kind, and the payload bytes. Nothing is signed, because
nothing is verified on this path; the constructor reads the action,
installation ID, repository, organization and sender out of the bytes the way
the receiver does, so the meta a handler sees is what the payload says. In
the quickstart's crate:

```rust,ignore
use octoevents::Match;

#[tokio::test]
async fn thanks_for_an_opened_issue() {
    let dispatcher = Dispatcher::<BoxError>::builder()
        .on((EventKind::Issues, Action::Opened), thank)
        .build();

    let envelope = Envelope::new(
        "delivery-1",
        EventKind::Issues,
        br#"{"action":"opened","sender":{"id":1,"login":"octocat"}}"#,
    );

    let outcome = dispatcher.dispatch(envelope).await;

    assert_eq!(outcome.matched, Match::Matched);
    outcome.result.unwrap();
}
```

`Envelope` cannot be built as a struct literal outside the crate, so a meta
cannot be paired with a payload that says something else. The target type and
ID come from headers, so they stay `None` unless assigned.

The receiver is tested with a signed synthetic request. `Verifier::sign`
gives the `Signature` GitHub would send for a body, which goes on the request
as the `X-Hub-Signature-256` value, so the test signs with the verifier the
receiver is built with; a request needs four headers, whose names
`octoevents::header` spells. `receive` takes any `http_body::Body` over
`Bytes`, and `String` is one, so the test needs no axum. With `http = "1"` as
a dev-dependency:

```rust,ignore
use octoevents::header;

#[tokio::test]
async fn accepts_a_signed_delivery() {
    let dispatcher = Dispatcher::<BoxError>::builder()
        .on((EventKind::Issues, Action::Opened), thank)
        .build();
    let verifier = Verifier::new(WebhookSecret::new("test-secret"));
    let webhook = WebhookReceiverBuilder::new(verifier.clone()).build(dispatcher);

    let body = r#"{"action":"opened","sender":{"id":1,"login":"octocat"}}"#;
    let request = http::Request::builder()
        .method("POST")
        .uri("/webhook")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::DELIVERY_ID, "delivery-1")
        .header(header::EVENT_NAME, "issues")
        .header(header::SIGNATURE, verifier.sign(body.as_bytes()))
        .body(body.to_string())
        .unwrap();

    let response = webhook.receive(request).await;

    assert_eq!(response.status(), 204);
}
```

Build the receiver over another secret and the same request is answered 401.
A verifier that also accepts a previous secret signs under its first.

## Transports

The receiver authenticates, bounds and dispatches one request; paths and
methods stay with your router.

**With the `tower` feature**, `WebhookReceiver` is a `tower_service::Service`
and mounts with `post_service`, as the quickstart does.

**Without it**, the receiver mounts on axum as a plain handler calling
`receive`, which takes any `http::Request` whose body yields `Bytes`:

```rust
use axum::{Router, extract::Request, routing::post};
use octoevents::{Dispatcher, Verifier, WebhookReceiverBuilder, WebhookSecret};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

let dispatcher = Dispatcher::<BoxError>::builder().build();
let webhook = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("development-secret")))
    .build(dispatcher);

let app: Router = Router::new().route("/webhook", post(move |request: Request| {
    let webhook = webhook.clone();
    async move { webhook.receive(request).await }
}));
```

**Without the `http-body` feature**, the core is sans-I/O: the verifier,
`Envelope::from_signed` over an `http::HeaderMap` and the body as `Bytes`, the
dispatcher and the wire format. That is the shape every surveyed Rust runtime
hands over: `lambda_http`, `spin-sdk` and `wstd` give an `http::Request`, `worker`
and `fastly` convert to one (`HeaderMap::from(&request.headers())` on a
Worker), and `aws_lambda_events` carries a `HeaderMap` in its event structs; a
consumer hand-parsing a raw invocation event collects its `(name, value)`
pairs into a `HeaderMap` and header-name case is `HeaderName`'s to handle. A
transport calls `from_signed` with the verifier, the map and the body, and
answers with an `http::StatusCode`: `ReceiveError::status` for a failure, 204
once the handler has succeeded, 500 when it has failed. The docs of
`from_signed` show, as code to copy, the three things the receiver does that
this path does not: refusing an unsigned request before reading the body,
bounding the body, and short-circuiting `ping`. `Dispatcher::dispatch` is a
plain `async fn` with no runtime of its own. The `worker` example runs the
receiver on Cloudflare Workers through `receive`.

## Tracing

With the `tracing` feature, a delivery runs in three spans:
`octoevents.receive` (INFO) around the receiver, `octoevents.verify` (DEBUG)
around the signature check, and `octoevents.dispatch` (INFO) around the
dispatcher, each recording an `outcome` on the way out. A failed delivery
emits one event at ERROR, `handler failed`, naming the delivery.

By default that event carries no text of the error, since the receiver places
no bound on the error type. `.trace_errors()` on the receiver builder puts an
`Error`'s text and source chain on it; `.trace_boxed_errors()` does the same
for a boxed error, or a `DispatchError` over one. Nothing secret-derived is
recorded anywhere. The full contract, span by span and field by field, is
[on the crate's front page](https://docs.rs/octoevents/latest/octoevents/#tracing).

## Security

- **Signed requests only.** A `Verifier` is required to build a receiver, so
  a deployment without a secret cannot be expressed. A request whose
  signature header is absent or malformed is refused before its body is read;
  a signed one is verified against the exact bytes GitHub sent. Only
  `X-Hub-Signature-256` is checked, never the SHA-1 header beside it.
- **Constant-time comparison.** Every configured secret is evaluated against
  a well-formed signature, a match included, so timing reveals neither the
  secret nor which one matched. The header is parsed into a `Signature`
  before any secret is used, so a malformed one never reaches the comparison.
- **Secret rotation.** GitHub holds one secret per webhook, so the rotation
  window is the verifier's: `Verifier::new(current).also(previous)` verifies
  against either. Deploy the new secret under `new` with the old one kept
  under `also`, change the secret in the webhook's settings, then drop the
  `also` once deliveries signed with the old one have drained. The order
  matters only to `Verifier::sign`, which signs under the first secret. A
  `WebhookSecret` is never empty, so a verifier over a guessable key cannot
  be expressed: `WebhookSecret::new` panics at construction, for a deployment
  that reads its secret at startup, and `str::parse::<WebhookSecret>` returns
  `WebhookSecretError::Empty` for one that reads it per request, where a
  panic is the wrong answer.
- **Bounded bodies.** The body is capped at GitHub's 25 MiB maximum before
  verification; `.body_limit(..)` on the receiver builder lowers it when your
  events are smaller.
- **JSON only.** Form-encoded deliveries are refused, so the bytes that were
  signed are the bytes that are decoded, and the payload is never re-encoded.
- **No replay protection.** GitHub signs no timestamp. Treat
  `EventMeta::delivery_id` as an idempotency key and deduplicate downstream;
  the policy seam is where that lives.
- **`ping` handled.** GitHub sends a `ping` when a webhook is created. A
  verified one is answered 204 before any handler runs; `.handle_ping(true)`
  passes it through instead. An unsigned one is 401 either way.
- **Nothing leaks.** The response is a bare status; the error's text reaches
  only the observer and, if asked, the tracing event. No span records the
  secret, the signature header, or a computed MAC.

## Migrating from Probot

The registrations map one to one. The rule that does not: handlers run one at
a time and the first error fails the delivery, where Probot runs every
matching handler and aggregates.

| Probot | octoevents |
| --- | --- |
| `app.on('issues.opened', h)` | `on((EventKind::Issues, Action::Opened), h)`, or `on(Action::Opened, h)` with the kind taken from `h`'s payload type. There is no string route form |
| `app.on('issues', h)` | `on(EventKind::Issues, h)`, or `on(AnyAction, h)` |
| `app.onAny(h)` | `always(h)`: first, for every delivery, over the envelope; its error fails the delivery. Sees `ping` only with `handle_ping(true)` |
| `app.onError(h)` | `on_error(h)` on the receiver builder, called with the meta and the receiver's handler's error. With a dispatcher as that handler, the error is a `DispatchError`: `{error}` says where (tier, handler, registration site), `error.source` says why. Unlike `onError`, it never sees a refused request: a bad signature is a 401, not an error |
| `app.receive(event)` | `dispatcher.dispatch(envelope)` with an envelope from `Envelope::new`; see [Testing without GitHub](#testing-without-github) |
| `context.payload` | The handler's input: a serde view of your own (`#[derive(Payload)]`), or octocrab's structs with the `octocrab` feature |
| `context.id`, `context.name` | `meta.delivery_id` and `meta.kind` on the `EventMeta`; a handler gets it beside the payload as `Event<P>` |
| `context.payload.action` | `meta.action`, an `Option<Action>`, read from the bytes before any handler runs |
| `context.payload.installation.id` | `meta.installation_id`, an `Option<u64>`; no octocrab needed, and enough on its own for a handler over `EventMeta` |
| `context.repo()` | `meta.repository`, an `Option<RepositoryRef>` with `id`, `name`, `full_name` and `owner`; `None` when the payload carries no complete `repository` object |

## Cargo features

| Feature | Default | Provides |
| --- | --- | --- |
| `http-body` | yes | `WebhookReceiver` and its builder, with `receive` over an `http::Request` whose body is an `http_body::Body` |
| `derive` | yes | `#[derive(Payload)]`, declaring a serde view's kind with `#[payload(EventKind::..)]`. Without it the same impl is three lines by hand |
| `tower` | no | `tower_service::Service` for `WebhookReceiver`, so it mounts with `post_service` |
| `octocrab` | no | `FromEnvelope` for octocrab's `WebhookEvent`, `Payload` for its per-kind structs, `Envelope::decode_event`. Makes octocrab's pre-1.0 types part of this crate's public API |
| `tracing` | no | The spans and the failed-delivery event under [Tracing](#tracing), and `trace_errors` / `trace_boxed_errors` on the receiver builder |

The core (envelope, verification, the handler trait and its inputs, the
dispatcher) depends on none of them and builds for `wasm32-unknown-unknown`.
`Envelope::from_signed` over an `http::HeaderMap`, the `header` constants and
`ReceiveError::status` as an `http::StatusCode` are part of it: the `http`
crate is not optional, since every surveyed Rust runtime hands over its types,
and it adds one entry to the dependency tree.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
