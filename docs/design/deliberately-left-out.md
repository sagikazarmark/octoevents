# Deliberately left out

Some requests come up in every webhook library, and some came up while this
crate's handler API was reviewed. The ones below are declined on purpose, so
the question is answered once. The evidence for the first five is in
[`../research/`](../research/), a survey of GitHub-webhook receivers in other
ecosystems ([`webhook-libraries.md`](../research/webhook-libraries.md)) and of
dispatcher designs in Rust
([`rust-dispatch-designs.md`](../research/rust-dispatch-designs.md)); the rest
were settled by compile probes during the pre-0.2.0 API review, and the
compiler's answer is quoted where it decided the matter. Where a type's own
docs also state the decision, the entry says so.

## No SHA-1 fallback

Only `X-Hub-Signature-256` is verified. GitHub sends the SHA-1
`X-Hub-Signature` beside it, and go-github falls back to that header when the
SHA-256 one is absent; here a request carrying only the SHA-1 header is
refused as unsigned, with `VerifyError::MissingSignature`. The stronger header
is always present to verify, so the fallback would protect no delivery and
would only let a sender choose the weaker algorithm. Recorded on `Verifier`.

## No form-urlencoded body

The webhook must deliver `application/json`; anything else is
`ReceiveError::UnsupportedContentType`. go-github also accepts
`application/x-www-form-urlencoded`, JSON under a `payload` form parameter
with the signature over the form body. Here `Envelope::raw` is both the signed
input and the payload every decode reads, and a form body would make it one
but not the other. Recorded on `Envelope::from_signed`.

## Kind from the header, not the payload's shape

`EventMeta::kind` is parsed from `X-GitHub-Event`, and a name this crate does
not know is `EventKind::Unknown` carrying the wire value, never a failure.
Inferring the kind from the payload's shape (octoapp's `#[serde(untagged)]`
event enum) mis-resolves when kinds share a shape (`issues` and
`issue_comment` both carry `issue`, `repository` and `sender`) and has no
answer for a kind it was not built with; every other library surveyed reads
the header. Recorded on `EventKind`.

## Consumer-defined views, not one blessed struct per kind

A `Payload` is any serde type that declares its kind with `impl_payload!`, so
a handler names the fields it reads and nothing else. go-playground/webhooks
ships one hand-written struct per kind, and its issue tracker is a record of
fields those structs lack and per-action variance they cannot follow.
GitHub's payloads differ by action and gain fields over time, so a library's
struct per kind is perpetually behind, while a view changes nothing when a
field it does not name appears or disappears. octocrab's per-kind structs are
available as payloads behind the `octocrab` feature for handlers that want
the kind's full model; they mostly leave the top-level `installation`,
`sender`, `repository` and `organization` objects to octocrab's
`WebhookEvent`, so a handler that needs those reaches for `WebhookEvent` or a
view. Recorded on `Payload`.

## No priorities or propagation control

Handlers run in tier order, then registration order, and each can only
continue or fail: none can be moved ahead of an earlier registration, stop
the chain, or pass a delivery on as "not mine". Symfony's numeric priorities
and `stopPropagation`, and dptree's `ControlFlow::Continue`, were surveyed and
left out: the tiers cover what a webhook receiver needs, and matching decided
by handlers at run time would make the route table unable to say what it
routes. Recorded on `Dispatcher`.

## No short-circuit tier

Closely related, and asked for separately: a tier that can succeed *and* stop
routing, so that "persist, and answer a redelivery of a stored delivery ID
with success without running the handlers again" fits in `always`. A
`ControlFlow` return from a tier was considered and declined. A tier that can
skip makes "matched" a run-time decision of one handler rather than a
property of the route table, so the `Outcome` could no longer be trusted and
a strict `fallback` could no longer say what it rejects. The same policy is
expressed, with the outcome in hand, by a webhook handler that wraps
`dispatch`: it persists first, returns `Ok(())` for a duplicate without
calling `dispatch`, and reads `Outcome::matched` to dead-letter or forward an
unmatched delivery. That wrapper is the policy seam; the dispatcher only
routes. The `dispatcher` example shows it. Recorded on `Dispatcher`.

## No raw tier and no meta handler

Both existed between 0.1.0 and 0.2.0 and were removed before release. The
raw tier ran before `always` and received the envelope, bytes included, so
that persistence could live in a tier; the meta handler received the
`EventMeta` alone, so that `always` and `fallback` could run for a payload
nothing can decode without handing over bytes the handler would not read.

Neither earned its place. The one production-shaped user left the raw tier
empty: persist-and-skip-on-duplicate cannot be expressed in a tier (see the
previous entry), so persistence moved into the wrapper along with
deduplication and dead-lettering, and a tier that carries bytes to a place
that cannot use them for the one thing they are needed for is a tier without
a job. The meta handler hid bytes from a handler that would not read them
anyway, at the cost of a trait, an adapter, an erased-handler variant and a
paragraph in every doc; an `Envelope` clone is an `EventMeta` clone plus a
refcount bump on the bytes, so nothing was saved. Letting `always` and
`fallback` take webhook handlers covers both, and `()` as a `FromEnvelope`
input covers the other thing the meta handler was for: a handler routed by
kind and action that decodes nothing and receives only the meta.

## No `TryFrom<Envelope>` as the decode bound

The event handler's input is bounded by the crate's own `FromEnvelope` rather
than by `TryFrom<Envelope>`, which would have read as std interop. Probed and
rejected on three grounds.

The blanket for every `Payload` is forbidden by the orphan rule:

```text
error[E0210]: type parameter `T` must be covered by another type when it
appears before the first local type (`Envelope`)
  |
  | impl<T: Payload> TryFrom<Envelope> for T {
  |      ^ uncovered type parameter
```

`TryFrom` is a foreign trait and `T` is uncovered, and the impl would in any
case overlap core's `impl<T, U: Into<T>> TryFrom<U> for T`. So `Payload` and
the handler bound would decouple: `impl_payload!` could emit a `TryFrom` impl
per type, but a hand-written `impl Payload` would not be a valid handler
input, and kind-from-type would hold for some payloads and not others.

A std trait cannot carry the `#[diagnostic::on_unimplemented]` note that
tells a consumer with a serde type to call `impl_payload!`, or with a
cross-kind view to implement the trait directly. And `TryFrom<Envelope>`
names a general conversion, while the bound has a specific contract: decode
the payload for a handler, checking the kind first for a `Payload`, and
report a `DecodeError` the dispatcher attributes to that registration.

Emitting `TryFrom<Envelope>` impls from `impl_payload!` as interop, beside
the bound rather than as it, remains possible and is not planned.

## Not `Event` as the bound's name, nor `EventDecoder`

`FromEnvelope` was nearly `Event`, so that `EventHandler<P: Event>` would
read naturally. Rejected: the blanket `impl<T: Payload> Event for T` would
read "every payload is an event", inverting GitHub's containment, in which
an event *has* a kind, an action and a payload. The vocabulary elsewhere in
the crate keeps that direction. `EventDecoder` was rejected because the
`-er` suffix names the agent that performs the decode, and the type
parameter `P` is the subject: the thing decoded, not the thing decoding.
`FromEnvelope` follows `FromStr` and `FromIterator`: it says where `P` comes
from.

## No sealed decode bound

`FromEnvelope` is open. Sealing it would let the crate add a method later
without a breaking change, at the cost of making every cross-kind consumer
view impossible without a second macro or octocrab. The trait has one
function, taking the envelope by reference and returning the input or a
`DecodeError`, and that shape is the contract; a consumer with logic that
spans kinds implements it for a view over the fields those kinds share and
registers the handler under several kinds with `on`, with no octocrab in the
picture. Recorded on `FromEnvelope`.

## No single generic handler trait

One trait `Handler<I>` over the input, with `Handler<Envelope>` for webhook
handlers and `Handler<(EventMeta, P)>` for event handlers, was probed and
compiles: with the input as a trait parameter rather than an associated type,
the closure blankets for the two shapes do not overlap. It was rejected on
ergonomics. A struct implementing the typed shape degrades to a tuple
argument, `async fn handle(&self, (meta, payload): (EventMeta, P))`, where
the two-trait design writes `handle(&self, meta: EventMeta, payload: P)`;
and a forwarding trait that restores the two-parameter signature over the
single trait overlaps the closure blanket again. Two traits over a shared
decode bound cost one extra trait and buy the signature every handler
writes.

## Deferred, not declined

The registration bound `E: From<H::Error>` makes an application error
absorb every handler's error through `From`, so a reusable handler struct
keeps its own error type. It is also why a handler that cannot fail needs
`impl From<Infallible> for AppError`, and why a bare `Ok(())` in a closure is
ambiguous (E0283) once the application error has two `From` impls. The
alternative, `H::Error = E`, removes both at the cost of the reusable-handler
case. The decision is deferred: writing handlers as `async fn` items
returning `Result<(), AppError>` needs neither the `Infallible` impl nor the
annotation, the README leads with that shape, and no shipped example or
rustdoc example declares `Infallible` any more. Re-evaluate at the next
persona review ([`../review/`](../review/)). With no crate-taught
`Infallible` left, a `From<Infallible>` or ambiguous-`From` hit that a
persona reports traces to a shape the user chose, and that is the data the
decision needs.
