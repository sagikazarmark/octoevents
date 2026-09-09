# Deliberately left out

Some requests come up in every webhook library, and some came up while this
crate's handler API was reviewed. The ones below are declined on purpose, so
the question is answered once. The evidence for the first six is in
[`../research/`](../research/), a survey of GitHub-webhook receivers in other
ecosystems ([`webhook-libraries.md`](../research/webhook-libraries.md)) and of
dispatcher designs in Rust
([`rust-dispatch-designs.md`](../research/rust-dispatch-designs.md)), and for
the header seam in a survey of header abstractions and Rust runtimes
([`header-abstractions.md`](../research/header-abstractions.md)); the rest
were settled during the pre-0.2.0 API review, by compile probes, with the
compiler's answer quoted where it decided the matter, or by what the four
review personas ([`../review/`](../review/)) reached for and what they did
not. Where a type's own docs also state the decision, the entry says so.

## No SHA-1 fallback

Only `X-Hub-Signature-256` is verified. GitHub sends the SHA-1
`X-Hub-Signature` beside it, and go-github falls back to that header when the
SHA-256 one is absent; here a request carrying only the SHA-1 header is
refused as unsigned, with `SignatureError::Missing`. The stronger header
is always present to verify, so the fallback would protect no delivery and
would only let a sender choose the weaker algorithm. Recorded on `Verifier`.

## No form-urlencoded body

The webhook must deliver `application/json`; anything else is
`ReceiveError::UnsupportedContentType`. go-github also accepts
`application/x-www-form-urlencoded`, JSON under a `payload` form parameter
with the signature over the form body. Here `Envelope::raw_payload` is both the signed
input and the payload every decode reads, and a form body would make it one
but not the other. Recorded on `Envelope::from_signed`.

## No header view; `http::HeaderMap` is the header type (reversed)

`Envelope::from_signed` takes `&http::HeaderMap`, and the `http` crate is a
non-optional dependency. 0.1.0 shipped `HeaderView`, a struct of six optional
strings standing between the headers a consumer held and the constructor that
read them, built from a `HeaderMap` behind the `http` feature, from a string
map with `from_lookup`, or with setters, so that a transport without `http`
types could use the sans-I/O path. The survey in
[`header-abstractions.md`](../research/header-abstractions.md) found that
transport to be empty: `worker`, `lambda_http`, `aws_lambda_events`, `spin-sdk`
4 and 7, `fastly`, `vercel_runtime` and `wstd` every one depend on `http`
non-optionally and either are `http::Request` or convert to it with a `From`
impl; the repo's own Workers example already ran on `http` types. The only
string-map transport is a consumer hand-parsing the raw invocation JSON, and
`HeaderMap: FromIterator<(HeaderName, V)>` covers that in one line. The two
Rust verifiers that take a container (svix, standardwebhooks) take
`http::HeaderMap` and made `http` required; none takes a closure, a trait or
a string map. The crate itself is `bytes` and `itoa`, both already in the
no-default tree, so it is one more `cargo tree` entry, MSRV 1.57 against the
crate's 1.88, and about 22 KB of `wasm32` in isolation, near zero where the
host SDK links `HeaderMap`, which is every surveyed host.

What the view cost was the one failure it could produce: `from_lookup` passed
lowercase names and compared nothing, so a map that kept the sender's casing
answered `None` for the signature and every delivery was 401 with the secret
correct, silent until production; the type docs, the constructor docs and the
`header` module docs each spent a paragraph on it. `HeaderMap::get` matches
case-insensitively by construction, so the paragraphs and the failure are
gone together. Its redacting `Debug` went with it: nothing in the crate prints
headers, the consumer holds the unredacted map regardless, and `Signature`'s
redacted `Debug` protects the parsed value. The malformed-versus-missing
distinction it carried for the signature costs nothing without it:
`Signature::try_from` parses the `HeaderValue`'s bytes, so a value that is
not visible ASCII is `Malformed` (400), not `Missing` (401), the distinction
svix and standardwebhooks make and no GitHub-specific crate does. No
replacement builder, lookup trait or positional value parameters were added:
the survey found no precedent for any of them in Rust and no runtime that
would use them. The `header` constants stayed, as `HeaderName`s, for the two
uses that exist: a streaming transport's pre-body signature check and a test's
`http::Request::builder()`. Recorded on `from_signed` and on the `header`
module. The feature that gated the crate and the receiver together is
`http-body`, gating the receiver's body handling alone and named for what it
turns on in dependency terms; `receiver`, named for what it provides, was the
alternative and is deferred, not rejected.

## Kind from the header, not the payload's shape

`EventMeta::kind` is parsed from `X-GitHub-Event`, and a name this crate does
not know is `EventKind::Unknown` carrying the wire value, never a failure.
Inferring the kind from the payload's shape (octoapp's `#[serde(untagged)]`
event enum) mis-resolves when kinds share a shape (`issues` and
`issue_comment` both carry `issue`, `repository` and `sender`) and has no
answer for a kind it was not built with; every other library surveyed reads
the header. Recorded on `EventKind`.

## No header-only meta; the payload is probed at receipt

`EventMeta` carries five fields read from the payload (action, installation
ID, repository, organization, sender) beside the four from the headers, and
both envelope constructors read them when the envelope is built, before any
handler runs. Three shapes that would defer that read were considered and
declined: a meta holding the header fields alone, with the payload fields read
when a handler's input is `EventMeta` or `Event<P>`; a meta that reads them
lazily on first access; and a probe that runs only when the route table has a
route on an action.

The first does not defer what it sets out to. The dispatcher looks a route up
by `(kind, Option<action>)`, so the action has to be known before the
dispatcher knows which handler, and so which input, the delivery is for;
"read the action when the input asks for it" is circular. A header-only meta
moves the same read into the dispatcher, which then needs somewhere to put
the result. What could be deferred is the four fields routing does not use,
but they ride on the pass that reads the action, as borrowed slices of the
same document, and they are what makes a handler over the envelope useful
without a decode: `always`, `fallback`, the policy seam skipping a bot
sender or dead-lettering by repository, and the error observer all read them.
`Event<P>` cannot take its meta from `P` either: `P` is usually a consumer's
view without `sender` or `action`, so it would need a second pass where there
is now one. The lazy meta costs more than it saves: `EventMeta` is `Clone +
Eq + Hash + Serialize` plain data, and a cell inside it breaks equality,
makes the observer's `&EventMeta` trigger a parse, and complicates the serde
path a forwarded envelope is read back through. The route-table-driven probe
is the one variant that skips work, and it would trade away `action` and
`installation_id` on the dispatch span, the failed-delivery event and every
`DispatchError` for a saving no benchmark has yet shown matters; it stays
recorded as an idea in
[`rust-dispatch-designs.md`](../research/rust-dispatch-designs.md).

The promise the crate makes is therefore "no decode before a matched
handler's input asks for it", not "no read of the body": the probe is one
linear pass over the bytes, in the same order of work as the HMAC over the
same bytes, that keeps five top-level values and builds no model of the rest.
That is less eager than every receiver surveyed: octokit/webhooks.js and
Probot `JSON.parse` the whole body in `verifyAndReceive` and route on
`payload.action` (Probot's `context.payload` is a property over that parsed
object, not a call that parses), gidgethub `json.loads` it into
`event.data`, octocrab parses it into a `serde_json::Value` and then a typed
model, go-github unmarshals it into the typed struct. GitHub puts `action`
in the body rather than a header, so every receiver that routes on it reads
the body before routing; CloudEvents designed its context attributes to make
that unnecessary and GitHub did not adopt them
([`webhook-terminology.md`](../research/webhook-terminology.md)). Recorded on
`EventMeta`.

## Consumer-defined views, not one blessed struct per kind

A `Payload` is any serde type that declares its kind with `#[derive(Payload)]`,
so a handler names the fields it reads and nothing else. go-playground/webhooks
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
the handler bound would decouple: the `Payload` derive could emit a `TryFrom`
impl per type, but a hand-written `impl Payload` would not be a valid handler
input, and kind-from-type would hold for some payloads and not others.

A std trait cannot carry the `#[diagnostic::on_unimplemented]` note that
tells a consumer with a serde type to derive `Payload`, or with a
cross-kind view to implement the trait directly. And `TryFrom<Envelope>`
names a general conversion, while the bound has a specific contract: decode
the payload for a handler, checking the kind first for a `Payload`, and
report a `DecodeError` the dispatcher attributes to that registration.

Emitting `TryFrom<Envelope>` impls from the derive as interop, beside the
bound rather than as it, remains possible and is not planned.

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
`impl<P: Payload> Payload for Event<P>` lets a registration under actions
alone accept a handler over `P` or `Event<P>` under one bound, with no
helper trait to look through the wrapper. The four review personas compiled their programs against a
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

## No handler attribute macro; `#[derive(Payload)]` (reversed)

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
the user never wrote. Revisit the attribute only as a diagnostic aid in the
`#[debug_handler]` sense, if arity errors keep appearing in reviews.

`#[derive(Payload)]` was declined at the same time, on the ground that it
expands to the same three lines the `impl_payload!` macro wrote and that the
macro already reported a misspelled kind at the literal. That weighed what
the derive generates and not where it sits, and the decision was reversed
once the second was weighed. The kind is a fact about the view, like its
fields, and the derive puts it on the view, in the derive list every serde
user already reads, `#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::Issues)]`; the macro put it one statement away in a
grammar of its own, `Type => kind`, that nothing else in Rust uses. Same
line count, the same generated impl but for one bound (below), same rustc
error at a misspelled variant; a missing attribute is reported at the type
name and a malformed one at the token, in the derive's own words. What it
costs: a second crate, `octoevents-derive`, published beside this one and
pinned exactly, with `syn` and `quote` behind it, which every consumer with
`serde`'s derive already builds. It is the `derive` feature, on by default,
so a minimal build can leave it out and write the three lines by hand; the
`Payload` docs show that impl as what the derive expands to.

The one thing the derive adds to those three lines is `where Self:
DeserializeOwned`. `Payload` requires `FromEnvelope`, which a serde type has
through the blanket impl over `Payload + DeserializeOwned`, so an impl on a
generic view `View<T>` carrying only the type's own bounds owes
`FromEnvelope` for every `T`, deserializable or not, and rustc refuses it; a
hand-written generic impl needs the same clause. The bound is on every
expansion rather than only the generic ones, so the derive has one shape,
and so that a type that forgot `serde::Deserialize` is told the bound is
unsatisfied, at the `Payload` in its derive list, rather than that it
"cannot be decoded from an `Envelope`" with a note advising the derive it
already wrote. The bound names `::serde`, the name every consumer deriving
`serde::Deserialize` has; a hidden re-export of serde for a renamed one was
considered and not added, since the three-line impl already serves that
case.

`impl_payload!` was removed rather than kept beside the derive. It could
attach a kind only to a type the consumer owns, the same types the derive
reaches, since the orphan rule forbids an `impl Payload` for another crate's
type either way, so keeping it would have been a second spelling for the
same situation and nothing more. octocrab's per-kind structs keep their
crate-private macro, which exists for the shared rustdoc on each impl.

The attribute takes the kind as an expression, `#[payload(EventKind::..)]`,
and nothing else. An undocumented `#[payload(kind = EventKind::..)]` alias
was accepted for a while as a reservation: should the attribute ever carry a
second datum, the grammar would have a keyed place for it. It was removed
once the reservation was seen to reserve nothing: a keyed second argument
can be added after the positional kind, `#[payload(EventKind::.., other =
..)]`, with no break to the positional form, so the alias bought a parser
branch, an error message for a grammar nobody was told about and two tests,
and no future. `#[derive(FromEnvelope)]` stays declined: a cross-kind view's
decode is one line of the consumer's, `envelope.decode()`, and a derive would
have to guess it. Recorded on `Payload`.

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
where and lost the why for exactly the shape the front page teaches. The
cost is that an error type that is only `Display` (a `String`, a `&str`) has
no one-line path; a consumer with one writes an `on_error` observer that
emits its own event, and the receiver's bound-free event still names the
delivery.

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
which the tests cover) and a `DispatchError` over one. `AsRef<dyn Error +
Send + Sync>` rather than `Deref<Target = dyn Error + Send + Sync>` because
it is the conversion `anyhow` and the std pointers both implement by that
name and it says what is wanted, a view of the error, rather than how a
pointer is followed; a generic `Deref<Target: Error + ?Sized>` was probed
first and does not compile, since an unsized associated target cannot be
coerced to the `dyn Error` the event's `source` value needs. Two fields
rather than the whole error as one value, for every shape alike, because a
subscriber's error value must be `Error + 'static` and a `DispatchError`
over a box is no `Error`; a borrowed view that made it one would not be
`'static`. A `Display`-bounded third method reopens if a run meets an error
type that is only `Display`; `trace_boxed_errors` folds into `trace_errors`
if std ever implements `Error` for the unsized box, the impl the E0119 note
reserves.

The name `TracedError` was then taken for what does compile: a sealed trait
with the one blanket impl over `E: Error` and no second, which `trace_errors`
asks in place of `E: Error`. It admits exactly what the bare bound admitted,
and exists for its `#[diagnostic::on_unimplemented]`: the comparative review
(run 4) found that `.trace_errors()` on the front page's own
`Dispatcher<Box<dyn Error + Send + Sync>>` was a ten-line rustc report about
an unsized `dyn Error`, naming neither method. With the trait, and
`#[diagnostic::do_not_recommend]` on its blanket so rustc does not name the
impl in place of the message, the report is the crate's: "`DispatchError<Box<dyn
Error + Send + Sync>>` is not an `Error`, so `trace_errors` cannot record
it", with a note naming `trace_boxed_errors`. `Error` is its supertrait, so
the receiver reads the error through `Error` as before, and the method's
doctest holds the refusal as E0277. Recorded on `trace_errors` (the bound and
the `Display`-only cost), `TracedError`, `trace_boxed_errors` and
`BoxedError` (the shapes admitted, and the two fields).

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
the silence #18 set out to end. Recorded on `on_error` and in the `Tracing`
section of the crate front page.

## The transport's error text stays off the receive span

A request refused before any handler ran records the `ReceiveError` that
selected its status on the receive span, as `error`, so `bad_request` can be
told apart from `bad_request` (#70). Its `source()` is not recorded. Every
`ReceiveError` message is the crate's own fixed wording, so `error` can go on
the span unconditionally; the one source, `BodyError` beneath `BodyRead`, is
the transport's error rendered as text, which is the transport's to write and
could quote the request, signature included. Recording it would put text the
crate does not control on a span with no opt-in, where a handler's error text
waits for `trace_errors`. A setting for it (`trace_body_errors`, say) was not
added: the fixed text already says which refusal it was, and a transport's own
logging says why its stream broke. The text stays on the error value, where a
transport built on `Envelope::from_signed` that holds it decides. Recorded on
`record_refusal` in `receiver` and in the `Tracing` section of the crate front
page.

## The derive is suggested for `EventMeta`: a note cannot be filtered on `Self`

The single-trait redesign, #38, asked that `Payload`'s message, for an input
that declares no kind (`EventMeta`, `Envelope`), lead with "register it with
`on` and a matcher" and not suggest declaring a kind on a crate type. The
first half holds; the second is unmet, by the mechanism rather than by
omission. `assert_payload::<EventMeta>()` renders (abridged):

```text
error[E0277]: `EventMeta` is not a payload
  |
  = note: `Envelope`, `EventMeta` and a view over several kinds declare no kind: a handler over them is registered with `on` and a matcher that says the kind
  = note: a serde view over one kind declares it on the type: `#[derive(Payload)] #[payload(EventKind::..)]`
```

Stable `#[diagnostic::on_unimplemented]` takes `message`, `label` and `note`,
and every note fires for every `Self`. The filter that would confine the
second note to a serde type, `on(Self = "..", note = "..")`, belongs to the
compiler-internal `#[rustc_on_unimplemented]`; on the stable attribute rustc
refuses it:

```text
warning: malformed `diagnostic::on_unimplemented` attribute
  |
  |     on(Self = "EventMeta", note = "register a handler over it with `on` and a matcher"),
  |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ invalid option found here
  |
  = help: only `message`, `note` and `label` are allowed as options
```

Dropping the second note was declined: the derive hint is what a serde type
that never declared its kind needs, "the best diagnostic in the
crate" in #38's words (story 19), kept at the price of one inapplicable line
under a crate type. The note names the case it applies to, "a serde view
over one kind", and the line above it names `EventMeta` and `Envelope`
outright, so the reader with one can see which line is theirs. Reopens if
the `diagnostic` namespace gains a filter on `Self`. The `Payload` docs show
the `EventMeta` case and its first note.

## `Arc<H>` described on `Handler` only

Two closed tickets disagree. #38's documentation section asked the README's
Handlers section to show `Arc<H>` beside the struct form over `Event<P>`; the
rustdoc consolidation, #33, asked that the impl be described on the handler
trait only and that the front page and README stop mentioning it. #33 was
sequenced after #38, on which it waited, and wins: the impl is documented on
`Handler`, and the README and front page name `Arc` only as a test's shared
`Mutex`, captured by a closure. Beyond sequence, the evidence was on #33's
side. No persona used the `Arc<H>` impls in run 2 (0/4); neither the
receiver nor the dispatcher needs the caller's `Arc`, since each holds its
handlers behind its own and the receiver is `Clone` for any `H` without one;
and the impl's job, one struct shared between a route, a tier and a test
that reads its state, which is #38's user story for it, is met by the impl
and is a fact about the trait rather than a shape a first program needs.
Reopens if a persona reaches for `Arc<H>` and cannot find it. Recorded on
`Handler`.

## A payload view under a disagreeing `on` matcher fails at dispatch

`on((EventKind::Issues, Action::Opened), welcome)`, with `welcome` over a
view that declared `EventKind::PullRequest`, compiles; every delivery the
route matches then fails with `DecodeError::KindMismatch`, which names both
kinds and is attributed to that registration's site and handler. The
Probot-migrant persona, for whom `on((kind, action), view)` is the natural
spelling, asked for a compile error instead, or for the README to say there
is none (run 3). The README sentence was written; the compile error was
declined.

`on` is bound on `FromEnvelope`, and most of its inputs have no kind to
compare: `EventMeta`, `Envelope` and a view over several kinds are
registered under a matcher because the matcher is the only place their kinds
are said. A check for the inputs that do declare one, `I::KIND`, needs `on`
to do something different when `I` is a `Payload` than when it is not, and
the stable way to select behaviour by that distinction is a marker parameter
on the trait, the mechanism declined above. Even with it, the matcher is a
value built from `EventKind` at run time, and an enum is no stable const
generic, so the two kinds could be compared at the `on` call at the
earliest, not by the compiler. The spelling the compiler does check exists
and is one token away: `on(Action::Opened, welcome)` takes the kind from the
type and cannot be registered under the wrong one.
Reopens with the marker, which would move the check from the dispatch to the
registration call. Recorded on `on` and in the Routing section of the
README.

## One `on`; `on_payload` and `on_payload_action` removed

`on_payload(handler)` and `on_payload_action(actions, handler)` registered a
handler over a `Payload` under the kind its type declared, for every action
or for the listed ones. Beside `on(matcher, handler)` they made three
registration methods for one routed tier, and the split was by input rather
than by what the registration said: `on_payload_action([Action::Opened],
label)` and `on((EventKind::Issues, Action::Opened), label)` registered the
same route, one with the kind said once and one with it said twice, under
different names. The Probot-migrant persona read that as `issues.opened`
"spelled across three places" (run 3), and the imbalance between `on` and
`on_payload` was the friction that reopened the question.

The two methods existed because `on`'s matcher, `impl Into<EventMatcher>`,
could not see the handler's input type, so a matcher of actions alone had no
kind to fill in. The matcher bound is now `IntoMatcher<I>`, generic over the
input. Every shape that converts into an `EventMatcher` implements it for
any `I` and says its kinds, as before. `Action`, `[Action; N]` and
`AnyAction`, a unit struct, implement it only where `I: Payload`, and take
the kind from `I::KIND`. So `on(Action::Opened, label)` is the former
`on_payload_action`, `on(AnyAction, notify)` the former `on_payload`, and
`on` is the one verb for the routed tier: *on (event and/or action), do
this*. What the two methods checked, `on` still checks: actions alone under
an input that declares no kind fail to compile, and the message rustc
reaches is the `Payload` trait's own, whose first note says to spell the
kind. The blanket impl and the three relative impls are disjoint because
`Action`, `[Action; N]` and `AnyAction` do not convert into an
`EventMatcher`, which they cannot without a kind, and rustc confirmed the
coherence.

Three spellings for "every action of the declared kind" were weighed.
`on(notify)` alone is not expressible: no arity overloading, no default
arguments. `on(.., notify)`, `RangeFull` as the matcher, compiles and is
shortest, but is a pun that needs a sentence to say "all of what", and
"everything" invites confusion with `always`; declined. `Action::Any`, a
variant on the wire enum, was declined because `Action` is GitHub's value
verbatim, held by `EventMeta::action` and round-tripped through serde: a
variant no delivery can carry would give every `match` a dead arm and
`as_str` a string GitHub might one day send. `AnyAction` is a value with no
data whose name says what it selects, the pattern of `tower_http`'s
`cors::Any`, and it matches the slot it registers under (`Slot::any_action`).
A method beside `on` for this one case, keeping `on_payload` for it alone,
was the last alternative; declined because it would keep the imbalance for
the case the name least fits (what differs is "every action", not the
input).

What was given up: `on`'s signature gains a type parameter, `on::<I, H, M>`,
so a struct handling several inputs names it with one more `_`; the
`IntoMatcher` message cannot name the input, since rustc checks that bound
before it has inferred `I` from the handler, so it renders `_` and the text
does not try; and `on([], label)`, an empty action array, which the removed
method accepted as registering nothing, is E0283, since three array impls fit
`[_; 0]`. Nothing writes that on purpose. Net surface: two methods removed,
one trait and one unit struct added.

`IntoMatcher` is open, as `FromEnvelope` is (the entry above), and for the
same reason: sealing it would let the crate add a method without a breaking
change, at the cost of the consumer-defined matcher. The trait has one
function returning an `EventMatcher`, and that shape is the contract. A
consumer's matcher for any input is a `From<_> for EventMatcher`, which the
blanket picks up; one for payload inputs alone implements `IntoMatcher<I>`
under `I: Payload` and reads `I::KIND`, as the shipped relative shapes do,
and both compile beside the shipped impls (probed). Recorded on `on`,
`IntoMatcher` and `AnyAction`.

## `From`, not a `map_err` adapter, to convert a route's errors

A routed handler's error and the decode's `DecodeError` both become the
dispatcher's `E` through `From`: `on` asks `E: From<H::Error> +
From<DecodeError>`, `always` and `fallback` ask `E: From<H::Error>` alone.
The dispatcher survey ranked the other shape, tower's, as portable: a
`map_err`-style adapter at registration, a closure from the handler's error
to `E` handed in beside the handler, instead of a bound on `E`
([`rust-dispatch-designs.md`](../research/rust-dispatch-designs.md), the
tower section's portable ideas). Declined. `From` is what `?` converts
through and what `thiserror`'s `#[from]` exists to derive, so the conversion
a handler's body already uses to return its error is the one the dispatcher
uses to absorb it, and a handler written against `E` itself converts
through nothing at all. The adapter would be a second conversion mechanism
for the same error, spelled per registration, and the survey's own reason
for it, that tower services are combinators with no shared error type, does
not hold for a dispatcher whose one `E` is the point.

What the bound costs was measured: the platform persona's application error
needed one `#[from] DecodeError` variant (run 3), the quickstart's
`Box<dyn Error + Send + Sync>` needs nothing, and `anyhow::Error` and
`String` handlers register on it with no glue (the comparative review's
probe). An adapter would reopen for a handler whose error `E` cannot
convert from and the consumer cannot touch, a foreign type with no `From`
either side may write; a closure over the handler does that today.
Recorded on `Dispatcher` (the paragraph on `From` at registration) and `on`
(which asks `From<DecodeError>` and why `always` does not).

## `fallback` stays, with one job

No persona registered a fallback in run 2 or run 3: 0/4 in each of the two
runs since the raw tier and the meta handler were removed, 0/8 across them.
The one production-shaped user, whom strictness exists for, chose the policy
seam both times, because a fallback cannot see the match and dead-lettering
needs to: whether the kind was unknown to the route table, or only the
action, is in the `Outcome`, which only a handler wrapping `dispatch` reads
(the short-circuit entry above). The architect leaned to removing the tier
after run 2 and, after run 3, kept it as the first candidate to hide should
the next run read 0 again.

Kept, on three grounds. A cohort none of whom needed a strict fallback is
weak evidence against a feature. The survey found opt-in strictness
converged across the ecosystem: an unmatched delivery is a silent success by
default in octokit, Probot, gidgethub and go-github, and the strict
receivers are the exception. And the bot-builder persona's "nothing told me
a delivery arrived and went nowhere" (run 2) is the fallback's job,
undiscovered. It was re-documented rather than defended: its rustdoc says it
cannot see the match and points at the wrapper for dead-lettering, its first
example logs what nothing routed (`log_unrouted`) and its second rejects,
and the README's routing block no longer presents it as a peer of the routed
tiers. The trigger is set: the next run, a third, reading 0 hides it below
the policy seam, off the documented surface, its two examples rewritten on
the wrapper, which reads the outcome and can do both without the tier.
Recorded on `fallback`.

## No adapter from a routed handler to a `Handler<Envelope>`

The edge persona, building a receiver for one kind, asked for a
crate-provided adapter: a `Handler<Envelope>` made from a matcher and a
handler over `Event<P>`, so that one route needs neither a hand-written
adapter nor a dispatcher. His own count decided it. The hand-written adapter
(a struct generic over the inner handler, `PhantomData<P>`, decode then
forward) was 48 lines, 35 of code; a dispatcher with one action-routed
registration was 25 lines, 19 of code, and buys the
dispatch error (tier, handler name, registration site) and the outcome,
which the adapter would have to reinvent. The third shape, a handler over
`Envelope` that decodes its own view with `View::from_envelope(&envelope)`,
was 22 lines, and is the shape the `FromEnvelope` docs and the `Handler`
docs point the one-kind case at.
What the adapter would save, the dispatcher already saves, in fewer lines
and saying more.

What made the adapter expensive was not its length but a bound: rustc
suggested `Sync` for the inner handler, which compiled natively and failed
on `wasm32` with the crate's own "is not a handler over `Envelope`". The
`MaybeSync` supertrait on `Handler` (#46) removes that, so a consumer who
wants the adapter writes it as first written, on both targets. Reopens if a
run finds the one-route dispatcher itself the friction, now that the bound
no longer is. The `Handler` docs point the one-kind case at
`View::from_envelope`.
