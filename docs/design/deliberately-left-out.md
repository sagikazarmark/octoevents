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
expressed, with the outcome in hand, by a handler over the envelope that wraps
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
`fallback` take handlers over the envelope covers both, and `EventMeta` as a
`FromEnvelope` input covers the other thing the meta handler was for: a
handler routed by kind and action that decodes nothing and receives only the
meta.

## No `TryFrom<Envelope>` as the decode bound

A routed handler's input is bounded by the crate's own `FromEnvelope` rather
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

`FromEnvelope` was nearly `Event`, so that a handler bound `P: Event` would
read naturally. Rejected: the blanket `impl<T: Payload> Event for T` would
read "every payload is an event", inverting GitHub's containment, in which
an event *has* a kind, an action and a payload. The vocabulary elsewhere in
the crate keeps that direction; the name went instead to the struct
`Event<P>`, which is exactly that containment, the meta beside the payload.
`EventDecoder` was rejected because the
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

## One generic handler trait, not two (reversed)

One trait `Handler<I>` over the input, with `Handler<Envelope>` for the
receiver and the always and fallback tiers and `Handler<(EventMeta, P)>` for
routed handlers, was first probed and rejected on ergonomics: a struct
implementing the typed shape degraded to a tuple argument,
`async fn handle(&self, (meta, payload): (EventMeta, P))`, and a forwarding
trait restoring two parameters overlapped the closure blanket.

The single trait was then adopted with a named struct in place of the bare
tuple. `Event<P> { meta: EventMeta, payload: P }`, destructured in the
parameter as `Event { meta, payload }: Event<P>` or read as `event.meta` and
`event.payload`, gives the two halves names without a second trait, and a
handler that needs only the payload takes `P` alone with no dead meta
parameter. `Payload: FromEnvelope` as a supertrait with
`impl<P: Payload> Payload for Event<P>` lets `on_payload` accept a handler
over `P` or `Event<P>` under one bound, with no helper trait to look through
the wrapper. The four review personas compiled their programs against a
prototype with no crate-caused failure at a registration. What was given up:
a two-argument `async fn(meta, payload)` is no longer a handler, and rustc
reports one passed to a registration method with its arity error (E0593)
rather than the trait's `on_unimplemented` hint; the README says that meta
and payload together is `Event<P>`. Recorded on `Handler` and `Event`.

## No argument-shape marker on the handler trait

`EventHandler<P, Args = (EventMeta, P)>`, a second type parameter naming the
closure shape a blanket covers, was probed on the two-trait design and is
coherent: it admits a payload-only `Fn(P)` blanket beside the two-argument
one, keeps struct impls unchanged through the default, forwards through
`Arc<H>`, and, because two impl candidates unify with the obligation, turns
the E0593 arity error on a wrong-shape closure into the crate's own E0277
message. It is also the only stable mechanism that does so: rustc emits E0593
whenever it can commit to a single impl candidate whose `Fn` bound then fails
on arity, and `#[diagnostic::do_not_recommend]`, sealed helper bounds and
indirection through a helper trait all leave it in place (probed). Declined
as a second type parameter on the headline trait, visible in rustdoc, in
every registration signature and in rustc's `help` lines; the single trait
over one input reaches the same shapes with one parameter. The arity error on
a two-argument handler is the accepted cost. Recorded on `Handler`.

## No `()` input, and no `EventMeta` under a two-argument handler

`()` was a `FromEnvelope` that decoded nothing, so a meta-only route was
spelled `async fn revoke(meta: EventMeta, (): ())`. Removed with the single
trait: `EventMeta` is an input in its own right and the route is
`async fn revoke(meta: EventMeta)`. The one property `()` had over a view,
running for a body nothing can decode, cannot reach an action-routed slot
through `Envelope::from_signed`: a non-object body probes no `action`, so the
slot never matches, and the case existed only in a hand-built test.

`impl FromEnvelope for EventMeta` on its own, under the two-argument
`Fn(EventMeta, P)` blanket, was the first proposal for retiring `(): ()`. It
is coherent but delivers the meta twice, `async fn revoke(meta: EventMeta,
also_meta: EventMeta)`, and a one-argument blanket beside the two-argument
one is E0119; the one-argument shape needs either the marker above or the
single trait.

## No handler attribute macro, no derives

`#[octoevents::handler]` on an `async fn`, admitting any parameter list by
generating the trait impl, was prototyped (about 200 lines of `syn`, no new
third-party dependency, about half a second on a clean build). Declined on
what it generates rather than what it costs: a fn item can only become a
handler through an `Fn` blanket, so the macro turns the fn into a unit struct
of the same name, which makes `label(pr).await` in a test an error and lists
the fn as a struct in rustdoc; the tier a handler belongs to is decided by
the spelling of a parameter type, so `use Envelope as Env` silently unfits
it for `always`; and errors inside the generated impl are reported several
times at the attribute, with the crate's own notes advising a struct impl
the user never wrote. `#[derive(Payload)]` and `#[derive(FromEnvelope)]`
expand to the same lines `impl_payload!` and a three-line impl write, and
`impl_payload!` already reports a misspelled kind at the literal. Revisit the
attribute only as a diagnostic aid in the `#[debug_handler]` sense, if arity
errors keep appearing in reviews.

## No generic no-kind-check input, no `Deref` on `Event`

`View<T>`, a wrapper making any serde type a `FromEnvelope` through the
kind-free `Envelope::decode`, was prototyped and is coherent. None of the
four simulated users reached for it, three called it noise in the list of
inputs, and a view over several kinds is a three-line `FromEnvelope` impl
that keeps the crate's open seam in view. Deferred: it is additive, and can
ship if demand appears.

`Deref<Target = P>` on `Event<P>` was probed and dropped: the payload has a
name, `payload`, and the deref would shadow a view field named `meta` or
`payload` behind a type-safe but surprising resolution. Recorded on `Event`.

## `Error`, not `Display`, to trace the error's text; a second method for the boxed shape

With the `tracing` feature a failed delivery is one ERROR event, and the
error's text goes on it through `WebhookReceiverBuilder::trace_errors`, which
asks `E: Error`. The spec that introduced the text (#29) named a `Display`
bound. `Error` was chosen because the text alone is not the story: a
`DispatchError`'s `Display` says where the delivery failed (the tier, the
handler, the registration site) and why is its `source()`, the application
error, which a `Display` bound cannot reach. The event records both, `error`
as the text and `source` as an error value the subscriber renders with the
chain beneath it, and a `Display`-bounded method would have recorded the
where and lost the why for exactly the shape the front page teaches.

That shape, `Box<dyn Error + Send + Sync>`, is not an `Error` (std implements
`Error` for `Box<E>` only for a sized `E`), so a `DispatchError` over it is
not one either, and `trace_errors` refuses both. One method over a crate
trait covering both an `E: Error` and the box was probed and is not
expressible:

```text
error[E0119]: conflicting implementations of trait `TracedError` for type `Box<(dyn std::error::Error + Send + Sync + 'static)>`
   |
 7 | impl<E: Error + 'static> TracedError for E {
   | ------------------------------------------ first implementation here
...
11 | impl TracedError for Box<dyn Error + Send + Sync> {
   | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ conflicting implementation for `Box<(dyn std::error::Error + Send + Sync + 'static)>`
   |
   = note: upstream crates may add a new impl of trait `std::error::Error` for type `std::boxed::Box<(dyn std::error::Error + std::marker::Send + std::marker::Sync + 'static)>` in future versions
```

The same note defeats every variant: an impl for `DispatchError<Box<dyn
Error + Send + Sync>>` beside the blanket, a blanket over `Deref<Target:
Error>` beside one over `Error` (they overlap for real on `Box<AppError>`),
or a second `Error` impl for `DispatchError` over the box. The blanket over
`Error` is what every `thiserror` enum needs, so the box gets its own method,
`trace_boxed_errors`, over the sealed `BoxedError`: anything
`AsRef<dyn Error + Send + Sync + 'static>` (`Box`, `Arc`, `anyhow::Error`,
`eyre::Report`) and a `DispatchError` over one. `AsRef` rather than `Deref`
because the target must be a concrete `dyn Error` to become the event's
`source` value: a generic `?Sized` `Deref::Target` cannot be coerced to one.
Two fields rather than the whole error as one value, for every shape alike,
because a subscriber's error value must be `Error + 'static` and a
`DispatchError` over a box is no `Error`; a borrowed view that made it one
would not be `'static`. Recorded on `trace_errors`, `trace_boxed_errors` and
`BoxedError`.

## No `trace_error` observer

The text was first an `on_error` observer, `trace_error`, that emitted a
second ERROR event, `handler error`, beside the receiver's bound-free
`handler failed`. A failed delivery was two lines that read alike, and the
receiver could not make them one: its own event needs no bound and the
observer it holds is a type-erased `Fn`, so it cannot tell that the observer
about to run will say everything it is about to say. Making the text a
receiver setting instead (`trace_errors`, `trace_boxed_errors`) lets the
receiver emit one event that carries it, and leaves `on_error` for what
tracing does not do: a metric, a dead letter, a line on stderr. The two
compose, and the observer changes nothing about the event. Suppressing the
receiver's event whenever any observer is registered was declined: a
metrics-only observer would have made failed deliveries invisible at ERROR,
the silence #18 set out to end.

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
