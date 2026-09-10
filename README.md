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
use octoevents::{Action, BoxError, Dispatcher, Envelope, EventKind, Verifier, WebhookReceiverBuilder, WebhookSecret};

/// Runs for `issues.opened`. The envelope is the verified unit of receipt: its
/// meta (delivery ID, kind, action, repository, sender, ...) and the raw payload.
/// `BoxError` is the crate's erased error; any `Error + Send + Sync + 'static` converts into it with `?`.
async fn thank(envelope: Envelope) -> Result<(), BoxError> {
    let sender = envelope.meta.sender.map(|s| s.login).unwrap_or_default();
    println!("Thank you for your contribution, @{sender}! :)");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;

    // Routes each verified envelope by kind and action.
    let dispatcher = Dispatcher::builder()
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
for a handler's own error type under [Error handling](#error-handling); the
quickstart itself reads the envelope's meta, returns the crate's `BoxError`,
and needs neither.

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
| 400 | The signature is malformed, a required delivery header is missing, the content type is not JSON, or the body could not be read |
| 413 | The body is over the limit (25 MiB by default) |

A 500 is a bare status: with the `tracing` feature the receiver emits one
event at ERROR per failed delivery, error and cause chain included, as
[Tracing](#tracing) describes, and an observer registered with `on_error` is
where the error reaches your code, as [Error handling](#error-handling)
shows.

The dispatcher is optional. `WebhookReceiverBuilder::new(verifier).build(thank)`
builds the receiver around the handler alone, and `thank` then receives every
verified delivery, whatever its kind, except the `ping` the receiver answers
itself by default; the `axum` example is that shape.

`thank` took the whole envelope; a handler can take the decoded payload
instead, and a dispatcher can run several handlers in three tiers, always,
route and fallback. The next sections cover both.

## Try it

[`gh webhook forward`](https://docs.github.com/en/webhooks/testing-and-troubleshooting-webhooks/using-the-github-cli-to-forward-webhooks-for-testing)
creates a temporary webhook on a repository you administer and forwards its
deliveries (exact bytes and headers, signature included) to localhost.

In one terminal, start a receiver. The `quickstart` example is the program
above:

```console
GITHUB_WEBHOOK_SECRET=development-secret cargo run --example quickstart --features tower
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

The other examples build on the quickstart, in this order, and each carries
tests that drive it without GitHub (`cargo test --example <name>` with the
same `--features`):

- `axum`: a receiver around one struct handler and no dispatcher, the
  simplest shape.
- `dispatcher`: every tier and a handler over every input, behind a receiver
  with its knobs set (`body_limit`, `handle_ping`, `on_error`, a rotation
  window with `Verifier::also`). Forward `issues` events to it.
- `policy_seam`: the production shape, a persisting, deduplicating,
  dead-lettering handler wrapping a dispatcher that routes octocrab's
  payloads. Forward `pull_request` events to it; a repository webhook, which
  is what `gh webhook forward` creates, carries no installation ID, so its
  labeler prints a skip line where a GitHub App's deliveries would be labeled.

```console
GITHUB_WEBHOOK_SECRET=development-secret cargo run --example dispatcher --features tower
GITHUB_WEBHOOK_SECRET=development-secret cargo run --example policy_seam --features tower,octocrab
```

The `worker` example under `examples/worker` is the receiver on Cloudflare
Workers; a package of its own, with its own README.

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

"Decoded" is what is turned into the handler's input when its route runs,
and where a delivery can fail before the handler sees it. "Nothing" does not
mean the payload went unread. Both envelope constructors validate UTF-8 across
the bytes, then scan the JSON and keep five top-level values: `action`,
`installation.id`, `repository`, `organization` and `sender`. That read is
best-effort and never fails; invalid UTF-8 or malformed JSON syntax leaves
all five empty. Skipped strings are checked for escape syntax, but escaped
surrogates need not be paired until a string is decoded. The read runs before
any handler because `on` routes by the action, and the action is one of the
five. The rest of the document is skipped rather than decoded; the
[`EventMeta` docs](https://docs.rs/octoevents/latest/octoevents/struct.EventMeta.html)
state the read and its cost.

A `Payload` is a serde view over the fields a handler reads, declaring the
event kind it decodes with `#[derive(Payload)]` and `#[payload(EventKind::..)]`.
It fails only on the fields it names, so a field GitHub adds elsewhere in the
document changes nothing.

```rust
use octoevents::{Action, BoxError, Dispatcher, Envelope, Event, EventKind, EventMeta, Payload};

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

// Envelope: the meta and the raw payload, nothing decoded.
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

let dispatcher = Dispatcher::builder()
    .always(audit)                                          // every delivery
    .on((EventKind::Installation, Action::Deleted), revoke) // kind and action spelled here
    .on(Action::Opened, label)                              // kind from `IssueOpened`
    .on(Action::Opened, notify)
    .build();
```

Meta and payload together form one input, `Event<P>`, not two parameters.

### Struct handlers and closures

A handler with dependencies is a struct implementing `Handler<I>`; the
dependencies are its fields, borrowed through `&self` on every delivery. The
struct keeps its own error type, and the dispatcher boxes it where the
struct is registered:

```rust
use octoevents::{AnyAction, Dispatcher, Event, EventKind, Handler, Payload};

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

let dispatcher = Dispatcher::builder()
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
written for a handler over a whole event. octocrab goes in your own
`[dependencies]` to name its types.

```rust
use octocrab::models::webhook_events::{WebhookEvent, payload::PullRequestWebhookEventPayload};
use octoevents::{Action, BoxError, Dispatcher, Event, EventKind};

// octocrab's struct for one kind is a payload: the kind comes from the type.
async fn label(Event { meta, payload }: Event<PullRequestWebhookEventPayload>) -> Result<(), BoxError> {
    println!("{}: label PR #{}", meta.delivery_id, payload.number);
    Ok(())
}

// octocrab's event for any kind is an input; it declares no kind, so the
// matcher spells it.
async fn triage(Event { meta, payload }: Event<WebhookEvent>) -> Result<(), BoxError> {
    println!("triage {:?} on {:?}", meta.action, payload.repository.map(|repository| repository.name));
    Ok(())
}

let dispatcher = Dispatcher::builder()
    .on(Action::Opened, label)
    .on((EventKind::PullRequest, [Action::Opened, Action::Synchronize]), triage)
    .build();
```

The trade-off is whole-model decode. The struct names far more of the payload
than a handler reads, and an incompatible change to any field it names
(removed, renamed, retyped, or made null) fails the delivery, where a view
names the fields its handler reads and fails only on those; a field GitHub
adds fails neither. A test fixture for a struct is a complete payload.

Two gaps to know before choosing a struct for a kind. The per-kind structs
mostly omit the top-level `installation`, `sender`, `repository` and
`organization` objects: `EventMeta` carries their IDs and logins beside every
payload, and `WebhookEvent` carries them whole, so an `installation` handler
that needs the account or the permissions takes `Event<WebhookEvent>` or a
view. And many structs leave their main object as an untyped
`serde_json::Value` (`check_run`, `check_suite`, `workflow_run`,
`workflow_job`, `release`, `team` among them). The caveats are under
[Feature caveats](https://docs.rs/octoevents/latest/octoevents/#feature-caveats);
the `policy_seam` example routes octocrab's payloads.

**Known decoding limitation in octocrab 0.54.1:** `code_scanning_alert` and
`repository_advisory` fail as `WebhookEvent` or `Event<WebhookEvent>`, even
when their payloads fit the per-kind models. The decoder removes common
fields that those models require, resulting in `DecodeError::Json` and a
500 through the receiver before the routed handler runs. For these kinds,
use `CodeScanningAlertWebhookEventPayload` or
`RepositoryAdvisoryWebhookEventPayload`, optionally inside `Event`, or a
consumer-defined view. Those inputs decode the original payload directly.

## Dispatcher

A `Dispatcher` is itself a handler over the envelope. Per delivery it runs
three tiers in order:

| Tier | Runs | Receives | Registered with |
| --- | --- | --- | --- |
| Always | First, for every delivery | `Envelope` | `always` |
| Route | The handlers matching the kind and action, then those matching the kind | Any input | `on` |
| Fallback | Only when no route matched | `Envelope` | `fallback` |

Within a chain, handlers run in registration order; the route tier is two
chains, the action-specific one and then the kind-wide one, whichever was
registered first. The first error fails the delivery and ends the dispatch.
There are no priorities and no propagation control. The registrations from
`on`, keyed by kind and then by action, are the *route table*: a kind is
known to it when any route is registered for it.

### Routing

`on` takes a matcher and a handler over any input. A matcher that says its
kinds (a kind, several kinds, a kind with actions, or kind/action pairs)
routes any handler. For a handler over a payload type, the matcher may say
actions alone (an `Action`, an array of them, or `AnyAction` for every
action) and the kind comes from the type.

Selections within one `on` call are a union: duplicate kinds or actions
have no additional effect. A registration selecting both a kind and a
specific action runs once for that action, at its action-specific position
before the kind-wide chain, preserving registration order within each chain.
Separate `on` calls remain independent, even when they share an `Arc`-backed
handler or the same registration-site location.

```rust
use octoevents::{Action, AnyAction, BoxError, Dispatcher, Envelope, EventKind, EventMeta, Payload};

#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::Issues)]
struct IssueOpened { issue: Issue }
#[derive(serde::Deserialize)]
struct Issue { number: u64 }

async fn audit(envelope: Envelope) -> Result<(), BoxError> { Ok(()) }
async fn notify(issue: IssueOpened) -> Result<(), BoxError> { Ok(()) }
async fn label(issue: IssueOpened) -> Result<(), BoxError> { Ok(()) }
async fn revoke(meta: EventMeta) -> Result<(), BoxError> { Ok(()) }
async fn forward(envelope: Envelope) -> Result<(), BoxError> { Ok(()) }
async fn reject(envelope: Envelope) -> Result<(), BoxError> {
    Err(format!("unhandled event: {}", envelope.meta.kind).into())
}

let dispatcher = Dispatcher::builder()
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

A matcher that says its kinds is *absolute*; one that takes the kind from
the payload type is *relative*. `on((EventKind::Issues, Action::Opened),
label)` is the absolute spelling of `on(Action::Opened, label)`, with the
kind said twice. The two are compared at dispatch, not at compile time: an
absolute matcher that disagrees with the view's kind fails every delivery it
routes with `DecodeError::KindMismatch`. A relative matcher under a handler
whose input declares no kind (`EventMeta`, `Envelope`) is refused at compile
time, with a message that says to spell the kind.

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
  `dispatch` and reads its outcome: [the policy seam][policy-seam]. The
  `policy_seam` example shows one.
- The policy seam is also where an envelope is persisted before the response
  goes out. GitHub abandons a request after 10 seconds and does not redeliver
  a failed delivery on its own, so a handler that fails after the envelope is
  stored is recovered from the store; one that fails before it is stored is
  recovered only by asking GitHub to redeliver.

[policy-seam]: https://docs.rs/octoevents/latest/octoevents/struct.Dispatcher.html#the-policy-seam

### Outcome

`dispatch` reports an `Outcome`: whether the delivery matched, and the result
of the handlers that ran. The two are independent: a matched delivery can
fail, and an unmatched one succeeds unless an `always` or `fallback` handler
fails it.

| `outcome.matched` | Meaning |
| --- | --- |
| `Match::Matched` | At least one routed handler is registered for the kind, or the kind and action |
| `Match::UnmatchedAction` | The kind is registered, this action is not |
| `Match::UnmatchedKind` | The route table never registered the kind |

As the receiver's handler, the dispatcher keeps only the result; the outcome
is for a wrapper to read.

## Error handling

Every handler keeps the error type it has, and every registration asks one
thing of it: that it converts into `BoxError`, the crate's erased error
(`Box<dyn Error + Send + Sync>` natively, `Box<dyn Error>` on `wasm32`).
Every `Error + Send + Sync + 'static` type does, through std's blanket
`From`, and every `Error + 'static` on `wasm32`; so do `BoxError` itself,
`anyhow::Error`, `String` and `&str`. The quickstart's handler returns
`BoxError` and needs no error type of its own. The dispatcher boxes the
error where the handler is registered, so handlers with different error types
share one dispatcher and no enum joins them. A payload that does not fit a
routed handler's view is boxed the same way, as the `DecodeError` it is.

A failure is a `DispatchError`: the boxed error wrapped with the tier, the
delivery's ID, kind and action, the failing handler's name, and the source
location of the registration that put it there. Its text says *where*; its
source chain says *why*:

```rust
use std::error::Error as _;

use octoevents::{Action, BoxError, DispatchError, Dispatcher, EventKind, EventMeta, Payload, Verifier, WebhookReceiverBuilder, WebhookSecret};

#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::Issues)]
struct IssueOpened { issue: Issue }
#[derive(serde::Deserialize)]
struct Issue { title: String }

async fn label(issue: IssueOpened) -> Result<(), BoxError> {
    println!("label '{}'", issue.issue.title);
    Ok(())
}

/// Runs when a handler failed, before the 500 is answered: where, then why,
/// one cause per line.
fn report(_: &EventMeta, error: &DispatchError) {
    eprintln!("{error}");
    let mut cause = error.source();
    while let Some(error) = cause {
        eprintln!("  caused by: {error}");
        cause = error.source();
    }
}

let dispatcher = Dispatcher::builder()
    .on(Action::Opened, label)
    .build();

let webhook = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("development-secret")))
    .on_error(report)
    .build(dispatcher);
```

When an `issues.opened` payload lacks the `title` the view names, `report`
prints:

```text
delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the route tier at the handler `app::label` registered at src/main.rs:29:6
  caused by: payload could not be decoded
  caused by: missing field `title` at line 1 column 39
```

The response stays a bare 500 either way: it is GitHub's delivery record, not
a log. The `on_error` observer is synchronous. It receives the error exactly
as the receiver's handler returned it: a `DispatchError` when that handler is
a dispatcher, as above, or your own enum when it is a plain handler, matched
on without a downcast. It runs whenever the receiver's handler fails: with a
dispatcher, that includes a payload that could not be decoded for a routed
handler, as above. It never runs for a refused request, which is a status
code, or for a short-circuited `ping`, which reaches no handler.

**Your own error type.** A handler with dependencies usually has one, and
`thiserror` derives it; the dispatcher asks nothing more of it than `Error +
Send + Sync + 'static` (`Error + 'static` on `wasm32`).
Behind a `DispatchError` it is boxed, and a policy that wants it back
downcasts the source:

```rust
use octoevents::{Action, DecodeError, DispatchError, Dispatcher, Envelope, EventKind, EventMeta, Verifier, WebhookReceiverBuilder, WebhookSecret};

#[derive(Debug, thiserror::Error)]
enum LabelError {
    #[error("the GitHub API is unavailable")]
    Api,
}

async fn label(envelope: Envelope) -> Result<(), LabelError> {
    let _ = envelope;
    Err(LabelError::Api)
}

let dispatcher = Dispatcher::builder()
    .on((EventKind::Issues, Action::Opened), label)
    .build();

let webhook = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("development-secret")))
    .on_error(|_: &EventMeta, error: &DispatchError| {
        if let Some(LabelError::Api) = error.source.downcast_ref::<LabelError>() {
            // page the on-call
        } else if error.source.is::<DecodeError>() {
            // a view GitHub's payload no longer fits: a deploy, not a page
        }
    })
    .build(dispatcher);
```

## Testing without GitHub

A handler is tested through `dispatch` with an envelope from `Envelope::new`:
the delivery ID, the kind, and the payload bytes. Nothing is signed, because
nothing is verified on this path; the constructor reads the action,
installation ID, repository, organization and sender out of the bytes the way
the receiver does, so the meta a handler sees is what the payload says. In
the quickstart's crate:

```rust,ignore
use octoevents::{Action, Dispatcher, Envelope, EventKind, Match};

#[tokio::test]
async fn thanks_for_an_opened_issue() {
    let dispatcher = Dispatcher::builder()
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
use octoevents::{Action, Dispatcher, EventKind, Verifier, WebhookReceiverBuilder, WebhookSecret, header};

#[tokio::test]
async fn accepts_a_signed_delivery() {
    let dispatcher = Dispatcher::builder()
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
A verifier that also accepts a previous secret signs under its first. The
`quickstart` example carries both tests.

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

let dispatcher = Dispatcher::builder().build();
let webhook = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("development-secret")))
    .build(dispatcher);

let app: Router = Router::new().route("/webhook", post(move |request: Request| {
    let webhook = webhook.clone();
    async move { webhook.receive(request).await }
}));
```

**Without the `http-body` feature**, the receiver is still there, over the
headers and the body already read: `receive_bytes` takes the request's
`http::HeaderMap` and the body as `Bytes` and answers with the
`http::StatusCode`, the same contract as `receive` (the header-only refusal,
the body limit, `ping`, the handler, the observer, the tracing) for a
transport with no `http_body::Body`. That is the shape every surveyed Rust
runtime hands over (`lambda_http`, `spin-sdk`, `wstd`, `worker`, `fastly`,
`aws_lambda_events`); the
[`receive_bytes` docs](https://docs.rs/octoevents/latest/octoevents/struct.WebhookReceiver.html#method.receive_bytes)
say how each does. Driven from a test, with the headers and the body a
runtime would hand over built by hand:

```rust
use http::{HeaderMap, StatusCode};
use octoevents::{Bytes, Dispatcher, Verifier, WebhookReceiverBuilder, WebhookSecret, header};

#[tokio::main]
async fn main() {
    let dispatcher = Dispatcher::builder().build();
    let verifier = Verifier::new(WebhookSecret::new("development-secret"));
    let webhook = WebhookReceiverBuilder::new(verifier.clone()).build(dispatcher);

    // What the runtime hands over: the headers and the body, already read.
    let body = Bytes::from_static(br#"{"action":"opened","sender":{"id":1,"login":"octocat"}}"#);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    headers.insert(header::DELIVERY_ID, "delivery-1".parse().unwrap());
    headers.insert(header::EVENT_NAME, "issues".parse().unwrap());
    headers.insert(header::SIGNATURE, verifier.sign(&body).into());

    // What it wants back.
    let status: StatusCode = webhook.receive_bytes(&headers, body).await;
    assert_eq!(status, 204);
}
```

A transport that wants the envelope rather than the answer, to persist it
before any handler runs or to forward it to another service as the
[wire format](https://docs.rs/octoevents/latest/octoevents/struct.Envelope.html#wire-format)
(the one flat JSON document a serialized `Envelope` becomes), calls
`Envelope::from_signed` with the verifier, the `HeaderMap` and the body, and
answers a failure with `ReceiveError::status`; its docs say what the receiver
does around that call. `Dispatcher::dispatch` is a plain `async fn` with no
runtime of its own.

The [`worker` example](examples/worker) runs the receiver on Cloudflare
Workers through `receive`, as a GitHub App's receiver: its `always` tier
forwards each envelope the dispatcher is handed as the wire format, to an
object keyed by the installation ID. It is a package of its own, outside the
workspace, since it builds for `wasm32-unknown-unknown` alone; its README
says how to build and run it.

## Tracing

With the `tracing` feature, a delivery runs in three spans:
`octoevents.receive` (INFO) around the receiver, `octoevents.verify` (DEBUG)
around the signature check, and `octoevents.dispatch` (INFO) around the
dispatcher, each recording an `outcome` on the way out. A failed delivery
emits one event at ERROR, `handler failed`, naming the delivery and carrying
the handler's error as `error`, an error value whose source chain the
subscriber renders (the `fmt` subscriber prints `error=<where>
error.sources=[<why>, ..]`). Nothing secret-derived is recorded anywhere. The
full contract, span by span and field by field, is
[on the crate's front page](https://docs.rs/octoevents/latest/octoevents/#tracing).

## Security

- **Signed requests only.** A `Verifier` is required to build a receiver, so
  a deployment without a secret cannot be expressed. A request whose
  signature header is absent or malformed is refused before its body is read;
  a signed one is verified against the exact bytes GitHub sent. Only
  `X-Hub-Signature-256` is checked, never the SHA-1 header beside it.
- **Payload authentication.** GitHub's signature covers the exact payload
  bytes, not the delivery ID, event name, or target headers. Header-derived
  metadata alone must not authorize security-sensitive actions; use
  authenticated payload data or independently trusted configuration.
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
  `WebhookSecret` is never empty, so an empty key cannot be configured:
  `WebhookSecret::new` panics at construction, for a deployment
  that reads its secret at startup, and `str::parse::<WebhookSecret>` returns
  `WebhookSecretError::Empty` for one that reads it per request, where a
  panic is the wrong answer.
  Consumers must supply a high-entropy secret; nonempty validation does not
  establish secret strength.
- **Bounded payload length.** On `receive`, accumulated payload length never
  exceeds the configured limit, GitHub's 25 MiB maximum by default;
  `.body_limit(..)` on the receiver builder lowers it when your events are
  smaller. Buffer growth can reserve allocation capacity beyond the length
  and the limit. Transport-owned frames have separate allocation bounds
  controlled by the transport, so this is not an allocation-memory ceiling.
  On `receive_bytes`, the caller has already read the body; its length is
  checked against the limit before verification.
- **JSON only.** Form-encoded deliveries are refused, so the bytes that were
  signed are the bytes that are decoded, and the payload is never re-encoded.
- **No replay protection.** GitHub signs no timestamp. Treat
  `EventMeta::delivery_id` as an idempotency key to deduplicate GitHub
  redelivery downstream; the policy seam is where that lives. The ID is not
  signed, so this does not prevent an attacker from resubmitting a captured
  signed payload under a different ID.
- **`ping` answered.** GitHub sends a `ping` when a webhook is created. A
  verified one is answered 204 before any handler runs; `.handle_ping(true)`
  passes it through instead. An unsigned one is 401 either way.
- **Nothing leaks.** The response is a bare status; the error's text reaches
  only the observer and the tracing event. No span records the secret, the
  signature header, or a computed MAC.

## Migrating from Probot

The registrations map one to one. The rule that does not: handlers run one at
a time and the first error fails the delivery, where Probot runs every
matching handler and aggregates their errors.

| Probot | octoevents |
| --- | --- |
| `app.on('issues.opened', h)` | `on((EventKind::Issues, Action::Opened), h)`, or `on(Action::Opened, h)` with the kind taken from `h`'s payload type. There is no string route form |
| `app.on('issues', h)` | `on(EventKind::Issues, h)`, or `on(AnyAction, h)` with the kind taken from `h`'s payload type |
| `app.onAny(h)` | `always(h)`: first, for every delivery, over the envelope; its error fails the delivery. Sees `ping` only with `handle_ping(true)` |
| `app.onError(h)` | `on_error(h)` on the receiver builder, called with the meta and the receiver's handler's error. With a dispatcher as that handler, the error is a `DispatchError`: `{error}` says where (tier, handler, registration site), `error.source()` says why. Unlike `onError`, it never sees a refused request: a bad signature is a 401, not an error |
| `app.receive(event)` | `dispatcher.dispatch(envelope)` with an envelope from `Envelope::new`; see [Testing without GitHub](#testing-without-github) |
| `context.payload` | The handler's input: a serde view of your own (`#[derive(Payload)]`), or octocrab's structs with the `octocrab` feature |
| `context.id`, `context.name` | `meta.delivery_id` and `meta.kind` on the `EventMeta`; a handler gets it beside the payload as `Event<P>` |
| `context.payload.action` | `meta.action`, an `Option<Action>`, read from the bytes before any handler runs |
| `context.payload.installation.id` | `meta.installation_id`, an `Option<u64>`; no octocrab needed, and enough on its own for a handler over `EventMeta` |
| `context.repo()` | `meta.repository`, an `Option<RepositoryMeta>` with `id`, `name`, `full_name` and `owner`; `None` when the payload carries no complete `repository` object |

## Cargo features

| Feature | Default | Provides |
| --- | --- | --- |
| `http-body` | yes | `WebhookReceiver::receive` over an `http::Request` whose body is an `http_body::Body`, answering an `http::Response`; the receiver, its builder and `receive_bytes` are in the core |
| `derive` | yes | `#[derive(Payload)]`, declaring a serde view's kind with `#[payload(EventKind::..)]`. Without it the same impl is three lines by hand |
| `tower` | no | `tower_service::Service` for `WebhookReceiver`, so it mounts with `post_service`; implies `http-body` |
| `octocrab` | no | `FromEnvelope` for octocrab's `WebhookEvent` and `Payload` for its per-kind structs. Makes octocrab's pre-1.0 types part of this crate's public API |
| `tracing` | no | The spans and the failed-delivery event under [Tracing](#tracing) |

The core (envelope, verification, the handler trait and its inputs, the
dispatcher, the receiver over headers and bytes) depends on none of them and
builds for `wasm32-unknown-unknown`. `Envelope::from_signed` and
`WebhookReceiver::receive_bytes` over an `http::HeaderMap`, the `header`
constants and `ReceiveError::status` as an `http::StatusCode` are part of it:
the `http` crate is not optional, since every surveyed Rust runtime hands over
its types, and it adds one entry to the dependency tree.

## Developing

Run `just pr` for the same checks as CI. The development
shell provides the pinned Dagger CLI; Dagger supplies the Rust toolchains,
wasm target and cargo-hack in containers.

`dagger.toml` configures the upstream Rust module, with its
cargo-hack arguments in `Cargo.toml` under `workspace.metadata.dagger`:

- Native compilation checks every feature combination and all Cargo targets.
- Unit, integration, doc and example tests run with no features, defaults, all
  features and each feature alone (examples require their declared features).
- Clippy checks all Cargo targets across features, with warnings as errors.
  Rustdoc also runs across features with warnings as errors to check links.
- MSRV checks use the manifest's `rust-version` across the feature powerset
  and all Cargo targets, without requiring a local installation of that toolchain.
- wasm32 checks the library and test targets across every feature combination,
  including the non-`Send` handler tests, and checks the standalone Worker
  example with its own lockfile. These are compile checks, not wasm test runs.
- Formatting, dependency auditing and generated workflow freshness are checked.

`dagger check` runs the module's default checks. The additional Cargo target
and MSRV checks use `dagger api call`: the current Dagger beta deduplicates
multiple registrations of the same module, and check discovery cannot pass
function arguments. `just pr` and `.github/workflows/compatibility.yaml`
include those calls. The excluded Worker package is checked through the
module's container API, preserving access to its path dependency on this crate.

List default checks with `dagger check -l`, or select one, for example
`dagger check rust:test`. `just --list` lists the convenient subsets.
For the fast local loop, use `just core` or `just watch`; `just integration`
and `just docs` also run directly through the local Cargo toolchain.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
