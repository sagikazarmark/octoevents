# octoevents

Receiving-side GitHub webhook handling: turning an untrusted HTTP request into
a verified envelope and routing it to consumer handlers. The sending side
(queueing, retries) is a separate project (`octodelivery`) and its vocabulary
is deliberately kept out of this one; "redelivery" is the one sending-side
word this side needs, since a receiver observes one and must answer it.

## Language

**Envelope**:
The verified unit of receipt: exact payload bytes plus the routing metadata
extracted from headers and a best-effort payload probe. Composed of an
`EventMeta` and the raw payload, as the fields `meta` and `raw_payload`. The
crate produces envelopes and consumers read them: outside the crate one comes
from `Envelope::from_signed` (the receiving path, verified) or `Envelope::new`
(a test's path, unverified, the meta probed from the same bytes), never from
a struct literal, so the two halves cannot disagree at birth. The third way,
serde over the *wire format*, reads back an envelope a trusted transport
forwarded, meta as forwarded, neither verified nor probed.
_Avoid_: Delivery (reserved for the outbound `octodelivery` project), event (the decoded unit, `Event<P>`, is the envelope decoded for one handler), message

**EventMeta**:
The envelope's routing metadata without the payload bytes: delivery ID, kind,
action, installation ID, repository, organization, sender, target. An input
in its own right, for a handler routed by kind and action that reads no
payload; the `meta` half of `Event<P>`; and what the error observer receives
alongside the handler's error.
_Avoid_: Common (the former nested group; its name carried no meaning), header (it also holds probed payload fields), delivery (reserved for `octodelivery`), receipt (reads as acknowledgement, and sits too close to Receiver), context (implies ambient services; this is plain data), `Routing` or `RoutingMeta` as the type (delivery ID and sender are not routing; "routing metadata" in prose is fine, since the meta is what routing reads)

**RepositoryMeta, AccountMeta**:
The fields the probe keeps of one payload object, as the types
`EventMeta::repository`, `organization` and `sender` hold: a repository's
`id`, `name`, `full_name` and `owner`; an account's (a user's, an
organization's or an app's) `id` and `login`. The rule the names follow: *X
Meta* is the meta of X, the projection that routing and a policy read, never the
object GitHub sends, which a decoded payload holds. The ID is the identity a
policy keys on; the name or login can change under it. Plain structs, built
as literals or with `new`.
_Avoid_: `Ref` as the suffix (the former names; a Rust word for a borrow and a GitHub word for a git ref, `ref`, `ref_type`, `refs/heads/..`, and the types are neither), `Repository` or `Account` as the type (octocrab's names for the whole objects, live in a consumer's imports), summary, reference, projection as the type (prose for what the types are is fine)

**Receiver**:
The component that authenticates, bounds, and dispatches one HTTP request,
owning no routing of paths or methods. The type is `WebhookReceiver`, built
by `WebhookReceiverBuilder`: the `Webhook` prefix is the crate's rule for a
name that would otherwise collide in a consumer's imports (`Receiver` is a
channel end in std and tokio, `Secret` is `secrecy`'s), and no other type
carries it. Two entry points over one policy: `receive`, over an
`http::Request` whose body is read from the transport (`http-body` feature),
and `receive_bytes`, over the headers and the body already read, in the
core.
_Avoid_: Service (names the optional Tower impl, not the concept), endpoint, listener, `Receiver` as the type (the collision the prefix avoids)

**Handler**:
Consumer-owned code that handles one verified delivery, received as one
input: an `async fn` item, a struct implementing `Handler<I>`, a closure, or
an `Arc` of any of them.
Handlers *handle*; the receiver *receives*. One trait, named by what it
receives: the input type `I` is any `FromEnvelope`, and it says what the
handler gets and what is decoded for it: the `Envelope` (bytes included), the
`EventMeta` alone, a `Payload` view alone, or `Event<P>` for the meta beside
the payload. The receiver and the always and fallback tiers take a handler
over the envelope; a routed handler is over any input. Its error is its own,
`type Error` on the trait with no bound; where a handler is registered (`on`,
`always`, `fallback`, the receiver's `build`) the error is asked to be
`Into<BoxError>`, which every `Error + Send + Sync + 'static` is, and on
`wasm32` every `Error + 'static`. In prose, "a handler over
`Envelope`", "a handler over `Event<P>`".
_Avoid_: Callback, subscriber, webhook handler and event handler (the former two flavours; now one trait and an input type), typed handler, payload handler (a handler over a `Payload` is registered with `on` like any other; its type fixes the kind when the matcher gives actions alone), raw handler (raw named a removed tier), meta handler (removed; a handler over `EventMeta` receives only the metadata)

**BoxError**:
The crate's erased error, `octoevents::BoxError`: `Box<dyn Error + Send +
Sync>` on native targets, the shape tower, hyper and axum call by that name,
and `Box<dyn Error>` on `wasm32`, where a Worker's error holds a `JsValue`
and is neither. What every handler's error converts into where the handler
is registered, and what a dispatch error holds as its source. The one bound
the crate places on an error, `Into<BoxError>`, admits every `Error + Send +
Sync + 'static` type through std's blanket `From`, `BoxError` itself,
`anyhow::Error`, `String`, `&str` and `Infallible`; `()` and a bare struct
without an `Error` impl are refused at the registration. The alias, not the
spelled-out box, so a consumer's `Result<(), BoxError>` compiles for a
Worker and a native server alike, as `MaybeSend` does for a future.
_Avoid_: `BoxedError` (the removed bound `trace_boxed_errors` asked), `DynError`, boxed error as the type (prose for what the alias is, fine), application error for the box (the application error is what a handler returns; the box is what it becomes)

**Event**:
`Event<P>`: the envelope decoded for one handler, the `EventMeta` beside the
payload decoded as `P`, as the named fields `meta` and `payload`. Distinct
from Envelope, whose payload is bytes. Meta and payload together is one
input, destructured in the parameter: `Event { meta, payload }:
Event<IssueOpened>`. When `P` is a `Payload`, so is `Event<P>`, of the same
kind, so `on` under actions alone takes a handler over either.
_Avoid_: Decoded envelope, typed envelope, context (implies ambient services)

**Dispatcher**:
A handler that routes envelopes to other handlers by kind and action, in
tiers: the *always* tier, the matched routes, then the *fallback* chain.
Produces an *outcome*. Part of the crate's core with every registration
method; only octocrab's input types need the `octocrab` feature. A policy the
tiers cannot express (skip a duplicate, dead-letter an unmatched delivery)
lives in a handler over the envelope wrapping `dispatch`, the *policy seam*.
_Avoid_: Router (implies path/method routing, which stays with the caller)

**Policy seam**:
The handler over the envelope that wraps `Dispatcher::dispatch` and holds
the policy the tiers cannot express: persist first, answer a redelivery of a
stored delivery ID with success without dispatching, read the outcome to
dead-letter or forward an unmatched delivery. Where deduplication and
dead-lettering live; the dispatcher only routes. The `policy_seam` example
shows one.
_Avoid_: Middleware, interceptor, wrapper as the term (prose for what the seam is, fine), pre-dispatch hook

**Route table**:
The dispatcher's registrations from `on`, keyed by kind and then by action,
what `Match` is decided against: a kind is *known* to the table when any
route is registered for it. A *chain* is one tier's handlers in registration
order; the always and fallback tiers are chains and not in the table.
_Avoid_: Routing table (network vocabulary), registry, handler map

**Redelivery**:
GitHub's second attempt at a delivery, carrying the same delivery ID, sent
when an operator or the app's own automation asks for one, usually because
the first was not answered 2xx; GitHub never sends one on its own. The one
sending-side word this side needs: a receiver observes one and the policy
seam answers it, with success for a delivery it has stored, and the crate
itself never asks for one.
_Avoid_: Retry (the sender's act, `octodelivery`'s word), replay (an attacker's act, which the crate does not guard against; see Security), duplicate as the term for the attempt (a duplicate is what the policy seam finds)

**Always**:
The dispatcher tier that runs first, before routing, receiving the envelope,
bytes included, for every delivery the dispatcher is handed: not a `ping`
the receiver answered itself, nor a redelivery a wrapper answered before
calling `dispatch`. Its failure fails the delivery; it never counts as a
match, so a strict fallback still rejects kinds nothing else handles. It can
continue or fail but never skip.
_Avoid_: Global handler, middleware, raw (the removed tier that once ran before it)

**Fallback**:
The dispatcher chain that runs only when no routed handler matched,
receiving the envelope as the always tier does. It cannot see the match: it
runs alike for a kind the route table never registered and for an action
GitHub added to a kind it did, and the envelope does not say which. Empty by
default, so unmatched deliveries succeed.

**Tier**:
One of the three steps a dispatcher runs a delivery through, in order:
always, route (the matched routes, action-specific then kind-wide), fallback.
A *dispatch error* names the tier its failing handler ran in.
_Avoid_: Stage, phase (kept for decode versus handle inside one handler), layer (middleware vocabulary)

**Registration site**:
The source location of the call that registered a handler (`always`, `on`,
`fallback`), captured at compile time
through `#[track_caller]`. What a dispatch error points an operator at,
beside the handler name.
_Avoid_: Call site (ambiguous with the handler's own calls), origin, registered at (reads as a time in code; fine in prose)

**Handler name**:
The type name of a registered handler, `std::any::type_name` of what the
registration method received, recorded beside the registration site: a
function path for an `async fn` item or a struct, a `{{closure}}` path for a
closure. For an operator to read, not for code to match on; `type_name`
promises no stable string. The `handler` field of a dispatch error and of
the dispatch span.
_Avoid_: Handler ID, handler label, type name alone (says the mechanism, not what it names)

**Dispatch error**:
What a failed dispatch reports: the application error, boxed as a `BoxError`,
wrapped with the tier, the delivery's ID, kind and action, the handler name
and the registration site of the failing handler. Says where, not why; why is
its source, the boxed application error, which a policy that wants its own
type back downcasts (`source.downcast_ref::<AppError>()`). A decode failure is
reported at the handler that needed the decode, the `DecodeError` as the
source. The type is `DispatchError`, an `Error` whatever the handler's error
was, so a dispatcher nests as a route of another. What the error observer
receives when a dispatcher is the receiver's handler.
_Avoid_: Handler error (the application error inside it), failure (prose for the event, not the type), `DispatchError<E>` (the former generic shape; the source is always the box)

**Error observer**:
The function registered with `on_error` on the receiver builder, called with
the event meta and a reference to the handler's error, as the handler
returned it and before the receiver boxes it, after a handler fails and
before the 500 is answered. Synchronous, and never called for a receive
failure or a short-circuited ping. For what tracing does not do: a metric, a
dead letter, a line on stderr; it runs beside the failed-delivery event and
changes nothing about it.
_Avoid_: Error handler (it handles nothing; the response is unchanged), hook, middleware, trace_error, `trace_errors` and `trace_boxed_errors` (removed: an observer that emitted a second event, then two builder settings that put the error on the one event; the event carries it unconditionally now)

**Failed-delivery event**:
The one `tracing` event at ERROR the receiver emits when a handler fails,
`handler failed`: the event meta's identifying fields (delivery ID, event
name, and action and installation ID when the delivery has them) and the
handler's error, boxed, as `error`, an error value whose text and chain of
sources the subscriber renders (`error=<where> error.sources=[<why>, ..]`
under the `fmt` subscriber). Unconditional: one event, error included,
whether or not an observer is registered. `error` is the one field name
recorded in two forms, text alone on the receive span for a refusal and an
error value here; to every subscriber it is the error's text in both.
_Avoid_: Handler error event (the removed second event), log line (a subscriber's rendering of it), error event in lowercase (ambiguous with the `error` field; "ERROR event" and "event at ERROR" name the level and are fine), `source` as a field (the removed second field; the chain is under `error`)

**Outcome**:
What one dispatch reports: whether the delivery was matched, and if not,
whether its kind was known to the route table. Distinct from success: a
matched delivery can fail, an unmatched one can succeed.
_Avoid_: Status (reserved for HTTP), result (the Rust type)

**Match**:
A delivery matches when at least one routed handler is registered for its
kind, or its kind and action. Matching is decided by the route table, never
by a handler; the always and fallback tiers do not match.
_Avoid_: Hit, handled (a matched delivery may still fail)

**EventMatcher**:
The kinds and actions one dispatcher registration selects: a kind, several
kinds, a kind with actions, or kind/action pairs, expanding to slots of
`(kind, Option<action>)`. `on` takes any `IntoMatcher` for its handler's
input: those shapes, which say their kinds and fit any input, or, for a
handler over a `Payload`, actions alone, an `Action`, an array of them or
`AnyAction` for every action, the kind coming from the payload type. In
prose, a matcher that says its kinds is *absolute*; one that takes the kind
from the type is *relative*.
_Avoid_: Filter, selector, route (a route is what a matcher registers), wildcard (for `AnyAction`; it selects every action of one kind, not every delivery)

**Kind**:
The parsed identity of an event, as the `EventKind` enum.
_Avoid_: Category, type (the Rust keyword and GitHub's overloaded "event type")

**Event name**:
The raw `X-GitHub-Event` wire string before parsing. A *name* is unparsed; a
*kind* is parsed.

**Action**:
The payload's top-level `action` value, GitHub's sub-classification of an
event. GitHub's own term, used verbatim.

**Delivery ID**:
The `X-GitHub-Delivery` GUID identifying one delivery attempt; the consumer's
idempotency key. In prose, "delivery" names one attempt ("runs for every
delivery", "fails the delivery"); it never names the envelope or any type.

**Target**:
The resource the webhook is configured on, GitHub's *hook installation
target*, from the `X-GitHub-Hook-Installation-Target-Type` and `-ID` headers:
`integration` for a GitHub App, `repository` for a repository webhook,
`organization` for an organization webhook, as `EventMeta::target_type`
(a `TargetType`) and `target_id`. The one meta field pair read from headers
GitHub does not always send. Distinct from the `installation_target` event
kind, which reports a change to a target.
_Avoid_: Hook target, installation (the App installation, `installation_id`, is a different thing), owner, scope

**Refusal**:
A request answered before any handler ran, as a `ReceiveError` and the status
`ReceiveError::status` maps it to: unauthorized (401) for a signature that is
absent or does not match, bad request (400) for one that is malformed, a
missing required header, a content type other than `application/json` or a
body the transport could not read, payload too large (413) for a body over
the limit. Payload bytes that are not valid JSON are not a refusal: the probe
is best-effort, the envelope is built, and a handler over it runs; only an
input that decodes them fails, as a handler failure. The receive span's
`outcome` names the class and its `error` the refusal's text; the error
observer never sees one.
_Avoid_: Rejection, denial, failure (kept for a handler's), receive error as the concept (the type's name)

**View**:
A consumer-defined serde type naming only the fields its handler reads,
decoded from the payload and indifferent to every other field GitHub sends
or adds. A view over one kind declares it with `#[derive(Payload)]` and is a
`Payload`; a view over fields several kinds share implements `FromEnvelope`
itself with `Envelope::decode`. The crate's answer to one struct per kind.
_Avoid_: Model (octocrab's structs, the whole object), DTO, schema, projection (kept for the meta types)

**Wire format**:
The one flat JSON object a serialized `Envelope` becomes, the meta's fields
at the top level beside `raw_payload` as base64, for a trusted internal hop
to another service, which reads it back through serde, meta as forwarded and
nothing verified or probed. Serialized by the crate, read by anything.
_Avoid_: Serialization format (the mechanism), envelope format, transport format, message

**Verify**:
The mechanism: HMAC comparison of `X-Hub-Signature-256` against the body,
`Verifier::verify`, over a `Signature` already parsed. It decides `Mismatch`
and nothing else: whether the header is there is decided from the headers
(`Missing`), and whether it is a signature by parsing it (`Malformed`), both
before the verifier is asked. "Authenticate" is acceptable in prose for the
goal verification achieves.
_Avoid_: Validate (collides with schema validation, despite GitHub's docs)

**Signature**:
The `X-Hub-Signature-256` value parsed once, as the type `Signature`: the 32
MAC bytes, nothing else. A header value becomes one through
`TryFrom<&HeaderValue>`, the path `Envelope::from_signed` and the receiver
take, `TryFrom<&[u8]>` for the bytes in another shape, or `str::parse`, for a
consumer parsing a string in a test or their own early-out, and that parse is
the one origin of `Malformed`; `Display` renders the header value back and
`From<Signature> for HeaderValue` puts it on a request, `Debug` is redacted,
and it has no comparison, neither `PartialEq` nor `ConstantTimeEq`: the MAC
bytes are compared inside `Verifier::verify`, over every configured secret,
and that is the verification path the crate offers. A constant-time `==`
would be safe (`digest::CtOutput` has one) but would hand out a second path,
one secret and no verify span; comparing two rendered signatures as strings
stays possible, as it must for a printable value, and is a way around the
path, not one the crate offers. What
`Verifier::sign` produces and
`Verifier::verify` takes, so the verifier is handed a settled format and can
only mismatch. In prose, "signature" alone names the value once the header is
in context; "signature header" names the wire string before parsing, as
"event name" does for the kind.
_Avoid_: MAC or tag as the type (the bytes inside, not the parsed header value), digest (a hash, not a MAC), signature header as the type (the unparsed string), `HeaderValue` (the `http` type it arrives as and converts back into)

**Verifier**:
The component owning the configured secrets and performing signature
verification. Required to build a receiver, so a deployment without a secret
cannot be expressed. It also signs (`Verifier::sign`): the `X-Hub-Signature-256`
value GitHub would send for a body under its first secret, so a test of the
receiving side can put a synthetic request through the receiver it built.
That is a test aid, not a sending-side feature: the crate sends nothing, and
the sending side's vocabulary (queueing, retries) stays out.
Lives with the secret and the signature error in the `signature` module, the
one place the secret's bytes are read.
_Avoid_: Validator, authenticator, signer (a role the verifier plays for a test, not a component), signature verifier (nothing else at the crate root is verified, so the qualifier adds length and no meaning)

**WebhookSecret**:
The shared HMAC key configured on the GitHub webhook. GitHub's own term,
in full: the type is `WebhookSecret`, beside `WebhookReceiver`, so the
crate's name for the thing GitHub calls the webhook secret says which
secret, and so it does not collide with `secrecy::Secret` in a consumer's
imports. Never empty: an empty one is the unset-environment-variable failure
mode, not a configuration, and every constructor refuses it, `new` by
panicking, for a deployment that reads its secret at startup, and
`str::parse`, `TryFrom<Vec<u8>>` and `TryFrom<&[u8]>` with a
`WebhookSecretError`, for one that reads it per request or as bytes, so the
verifier has nothing left to check. In prose, "secret" alone is fine once the webhook is
in context.
_Avoid_: Token, `Key` or `SigningKey` as the type (it is a secret to GitHub and to the crate; "HMAC key" in prose, for what the bytes are to the MAC, is fine), `Secret` as the type (the former name; generic at the root and a live collision), signing secret (Stripe's and Svix's term; the crate's is GitHub's)

**SignatureError**:
Why a body did not authenticate: the `X-Hub-Signature-256` header was
`Missing`, `Malformed` (not `sha256=` and 64 hex digits), or a `Mismatch`
under every configured secret. Each variant has one origin: `Missing` is
decided from the headers, `Malformed` by parsing the header into a
`Signature`, `Mismatch` by `Verifier::verify`, in that order, so the verifier
is handed a parsed value and has no format left to refuse. Named for its
subject, the signature, not for the operation: two of its three variants are
decided before `verify` runs, so an error named after verifying misdescribed
most of itself. The `Signature` variant of `ReceiveError`. Distinct from
`WebhookSecretError`, a configuration failure found before any delivery
arrives.
_Avoid_: `VerifyError` (the former name), `MissingSignature` and `MalformedSignature` (the former variants; the type already says signature), authentication error (the goal, not the mechanism; also reads as GitHub App auth)

**Payload**:
The JSON document GitHub sends, in two states that two fields name: the
*raw payload* (`Envelope::raw_payload`), the exact bytes as they arrived,
undecoded and never re-encoded; and the payload decoded for one handler
(`Event::payload`). On the receiving path the raw payload is also the signed
input; form encoding is refused so the two stay the same bytes. As a type
(`Payload`), one kind's decoded payload, declaring the kind it belongs to;
octocrab's per-kind structs, consumer-defined serde views and `Event<P>` over
any of them are payloads; octocrab's `WebhookEvent` and a view over several
kinds are not.
_Avoid_: Body (reserved for the HTTP transport layer), raw unqualified (says unprocessed without saying of what, and named a removed tier; "raw payload" is the term)

**Probe**:
The best-effort read of the payload bytes that fills the payload-derived
fields of an `EventMeta` (action, installation ID, repository, organization,
sender) without decoding the rest of the document. Partial, and never fatal:
malformed JSON leaves every probed field empty, one malformed field clears
only itself, and the bytes are kept either way. It runs at receipt, for every
envelope, whatever input its handler will take: the dispatcher routes by the
action, and the action is in the payload, so the probe cannot wait for a
decode to ask for it. It is the one read of the payload before a decode, and
the reason "decodes nothing" never means "the payload went unread". An
implementation term for prose and internals, not an API: it runs inside both
envelope constructors and no public name says "probe". No library or spec
surveyed names this step (`docs/research/webhook-terminology.md`); the
alternatives to running it at receipt are in
`docs/design/deliberately-left-out.md`.
_Avoid_: Peek, sniff, extract (unqualified; "extracted" is fine in prose), decode (the full, fallible turn into a handler's input), parse (kept for the header-to-kind step), lazy or deferred meta (a shape considered and declined; the meta is complete when the envelope is)

**Decode**:
Turning an envelope into a handler's input, through `FromEnvelope`, the one
way to decode and spelled the same for every input, `P::from_envelope`: a
serde `Payload` type checks the kind and then decodes the bytes, `EventMeta`
decodes nothing and `Envelope` is a clone, `Event<P>` pairs the meta with
`P`'s decode, octocrab's `WebhookEvent` decodes into octocrab's model, and a
consumer type implementing `FromEnvelope` itself decodes as it sees fit, a
view over several kinds with the kind-free `Envelope::decode`, the one
decoding primitive on the envelope. A decode failure
is a `DecodeError` saying why (a kind mismatch, a JSON error, or the input's
own reason, `DecodeError::Input`, the one a consumer's impl returns for a
failure that is neither) and fails the delivery at the position of the handler
that needed it.
_Avoid_: Parse (kept for the header-to-kind and probe steps), deserialize (the serde mechanism, not the concept), `decode_payload` and `decode_event` (removed inherent spellings of `from_envelope`)
