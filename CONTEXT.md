# octoevents

Receiving-side GitHub webhook handling:
turning an untrusted HTTP request into a verified envelope and routing it to consumer handlers.
The sending side (queueing, retries) is a separate project (`octodelivery`)
and its vocabulary is deliberately kept out of this one; "redelivery" is the one sending-side word this side needs,
since a receiver observes one and must answer it.

## Language

**Envelope**: The unit of receipt: what arrived, the `WebhookMeta` read from the headers and the exact payload bytes,
as the fields `meta` and `raw_payload`.
Building one reads nothing of the payload and cannot fail; what the payload says is decoded later,
the `EventMeta` by the dispatcher.
Data, with no verification claim: an `Envelope` value proves nothing about how it was built.
A received one is trustworthy because it came from _authenticate_
(or the receiver, which is built on it), not because of its type.
Otherwise one comes from `Envelope::new` or a struct literal, which pair a `WebhookMeta` with bytes
(a test's path, or a transport that authenticated the request by its own means; unverified),
or from serde over the _wire format_, which reads back an envelope a trusted transport forwarded, nothing verified.
Verification authenticates the payload bytes, not the delivery ID, event name, or target headers.
Authorization uses authenticated payload data or independently trusted configuration;
delivery-ID deduplication handles GitHub redelivery, not adversarial replay under a different ID.
_Avoid_: Delivery (reserved for the outbound `octodelivery` project), event
(the decoded unit, `Event<P>`, is the envelope decoded for one handler), message

**EventMeta**: A delivery's routing metadata: delivery ID, kind, action, installation ID,
repository, organization, sender, target.
The `WebhookMeta` fields under the same names, beside the fields `EventMeta::decode` reads from the payload,
strictly, in GitHub's shape: a payload that does not fit is a decode error, not an empty field.
The dispatcher decodes it once per delivery, before the always tier, routes on its action,
and hands it to every input it builds, so no input decodes it again.
An input in its own right, for a handler routed by kind and action that decodes no view;
and the `meta` half of `Event<P>`.
_Avoid_: Common (the former nested group; its name carried no meaning), header
(it also holds payload fields),
delivery (reserved for `octodelivery`), receipt (reads as acknowledgement, and sits too close to Receiver), context
(implies ambient services; this is plain data),
`Routing` or `RoutingMeta` as the type
(delivery ID and sender are not routing; "routing metadata" in prose is fine, since the meta is what routing reads)

**RepositoryMeta, AccountMeta**: The fields `EventMeta::decode` keeps of one payload object, as the types `EventMeta::repository`,
`organization` and `sender` hold: a repository's `id`, `name`, `full_name` and `owner`; an account's
(a user's, an organization's or an app's) `id` and `login`.
The rule the names follow: _X Meta_ is the meta of X, the projection that routing and a policy read,
never the object GitHub sends, which a decoded payload holds.
In `WebhookMeta`, the webhook is the request GitHub sends, read from its headers;
the configured webhook it was sent for is the target.
The ID is the identity a policy keys on; the name or login can change under it.
Plain structs, built as literals or with `new`.
_Avoid_: `Ref` as the suffix (the former names; a Rust word for a borrow and a GitHub word for a git ref, `ref`,
`ref_type`, `refs/heads/..`, and the types are neither), `Repository` or `Account` as the type
(octocrab's names for the whole objects, live in a consumer's imports),
summary, reference, projection as the type (prose for what the types are is fine)

**Receiver**: The component that authenticates, bounds, and dispatches one HTTP request,
owning no routing of paths or methods.
The type is `WebhookReceiver`, built by `WebhookReceiverBuilder`:
the `Webhook` prefix marks a type whose bare name would collide in a consumer's imports (`Receiver`, `Secret`)
or say nothing on its own (`Meta`). `WebhookReceiver`, `WebhookSecret` and `WebhookMeta` carry it; no other type does.
(`Receiver` is a channel end in std and tokio, `Secret` is `secrecy`'s.)
Two entry points over one policy: `receive`, over an `http::Request` whose body is read from the transport
(`http-body` feature), and `receive_bytes`, over the headers and the body already read, in the core.
_Avoid_: Service (names the optional Tower impl, not the concept), endpoint, listener, `Receiver` as the type
(the collision the prefix avoids)

**Handler**: Consumer-owned code that handles one verified delivery, received as one input: an `async fn` item,
a struct implementing `Handler<I>`, a closure, or an `Arc` of any of them.
Handlers _handle_; the receiver _receives_.
One trait, named by what it receives: the input type `I` is any `FromEnvelope`,
and it says what the handler gets and what is decoded for it: the `Envelope`
(bytes included), the `EventMeta` alone, a `Payload` view alone, or `Event<P>` for the meta beside the payload.
Under a dispatcher every input is built from the envelope and the `EventMeta` the dispatcher decoded once.
The receiver and the fallback tier take a handler over the envelope; the always tier and a routed handler are over
any input, `Event<Envelope>` for the meta beside the bytes.
Its error is its own, `type Error` on the trait with no bound; where a handler is registered
(`on`, `always`, `fallback`, the receiver's `build`)
the error is asked to be `Into<BoxError>`, which every `Error + Send + Sync + 'static` is,
and on `wasm32` every `Error + 'static`.
In prose, "a handler over `Envelope`", "a handler over `Event<P>`".
_Avoid_: Callback, subscriber, webhook handler and event handler
(the former two flavours; now one trait and an input type),
typed handler, payload handler (a handler over a `Payload` is registered with `on` like any other;
its type fixes the kind when the matcher gives actions alone), raw handler (raw named a removed tier), meta handler
(removed; a handler over `EventMeta` receives only the metadata)

**BoxError**: The crate's erased error, `octoevents::BoxError`: `Box<dyn Error + Send + Sync>` on native targets,
the shape tower, hyper and axum call by that name, and `Box<dyn Error>` on `wasm32`,
where a Worker's error holds a `JsValue` and is neither.
What every handler's error converts into where the handler is registered, and what a dispatch error holds as its source.
The one bound the crate places on an error, `Into<BoxError>`,
admits every `Error + Send + Sync + 'static` type through std's blanket `From`, `BoxError` itself, `anyhow::Error`,
`String`, `&str` and `Infallible`; `()` and a bare struct without an `Error` impl are refused at the registration.
The alias, not the spelled-out box, so a consumer's `Result<(), BoxError>` compiles for a Worker
and a native server alike, as `MaybeSend` does for a future.
_Avoid_: `BoxedError` (the removed bound `trace_boxed_errors` asked), `DynError`, boxed error as the type
(prose for what the alias is, fine),
application error for the box (the application error is what a handler returns; the box is what it becomes)

**Event**: `Event<P>`: the envelope decoded for one handler, the `EventMeta` beside the payload decoded as `P`,
as the named fields `meta` and `payload`.
Distinct from Envelope, whose payload is bytes.
Meta and payload together is one input, destructured in the parameter: `Event { meta, payload }: Event<IssueOpened>`.
When `P` is a `Payload`, so is `Event<P>`, of the same kind, so `on` under actions alone takes a handler over either.
_Avoid_: Decoded envelope, typed envelope, context (implies ambient services)

**Dispatcher**: A handler that routes envelopes to other handlers by kind and action, in tiers: the _always_ tier,
the matched routes, then the _fallback_ chain.
Produces an _outcome_.
Part of the crate's core with every registration method; only octocrab's input types need the `octocrab` feature.
A policy the tiers cannot express (skip a duplicate, dead-letter an unmatched delivery) lives in a handler over the
envelope wrapping `dispatch`, the _policy seam_.
_Avoid_: Router (implies path/method routing, which stays with the caller)

**Policy seam**: The handler over the envelope that wraps `Dispatcher::dispatch`
and holds the policy the tiers cannot express: persist first,
answer a redelivery of a stored delivery ID with success without dispatching,
read the outcome to dead-letter or forward an unmatched delivery.
Where deduplication and dead-lettering and custom error reporting live; the dispatcher only routes.
A handler can inspect an error before returning it, retaining the envelope and awaiting storage when needed.
The `policy_seam` example shows one.
_Avoid_: Middleware, interceptor, wrapper as the term (prose for what the seam is, fine), pre-dispatch hook

**Route table**: The dispatcher's registrations from `on`, keyed by kind and then by action,
what `Match` is decided against: a kind is _known_ to the table when any route is registered for it.
A _chain_ is one tier's handlers in registration order; the always and fallback tiers are chains and not in the table.
_Avoid_: Routing table (network vocabulary), registry, handler map

**Redelivery**: GitHub's second attempt at a delivery, carrying the same delivery ID,
sent when an operator or the app's own automation asks for one, usually because the first was not answered 2xx;
GitHub never sends one on its own.
The one sending-side word this side needs: a receiver observes one and the policy seam answers it,
with success for a delivery it has stored, and the crate itself never asks for one.
_Avoid_: Retry (the sender's act, `octodelivery`'s word), replay
(an attacker's act, which the crate does not guard against; see Security),
duplicate as the term for the attempt (a duplicate is what the policy seam finds)

**Always**: The dispatcher tier that runs first, before routing, receiving any input,
the envelope (bytes included), the `EventMeta`, or `Event<Envelope>` for both,
for every delivery the dispatcher is handed whose meta decoded: not a `ping` the receiver answered itself,
nor a redelivery a wrapper answered before calling `dispatch`.
Its failure fails the delivery; it never counts as a match,
so a strict fallback still rejects kinds nothing else handles.
It can continue or fail but never skip.
_Avoid_: Global handler, middleware, raw (the removed tier that once ran before it)

**Fallback**: The dispatcher chain that runs only when no routed handler matched,
receiving the envelope, bytes included.
It cannot see the match: it runs alike for a kind the route table never registered and
for an action GitHub added to a kind it did, and the envelope does not say which.
Empty by default, so unmatched deliveries succeed.

**Tier**: One of the three steps a dispatcher runs a delivery through, in order: always, route
(the matched routes, action-specific then kind-wide), fallback.
A _dispatch error_ names the tier its failing handler ran in, and none when the meta failed to decode, before any tier ran.
The `Tier` enum is internal: the tier appears in error text and tracing output,
not as a public field consumers branch on.
_Avoid_: Stage, phase (kept for decode versus handle inside one handler), layer (middleware vocabulary)

**Registration site**: The source location of the call that registered a handler
(`always`, `on`, `fallback`), captured at compile time through `#[track_caller]`.
What a dispatch error points an operator at, beside the handler name.
_Avoid_: Call site (ambiguous with the handler's own calls), origin, registered at
(reads as a time in code; fine in prose)

**Handler name**: The type name of a registered handler, `std::any::type_name` of what the registration method received,
recorded beside the registration site: a function path for an `async fn` item or a struct,
a `{{closure}}` path for a closure.
For an operator to read, not for code to match on; `type_name` promises no stable string.
The `handler` field of a dispatch error and of the dispatch span.
_Avoid_: Handler ID, handler label, type name alone (says the mechanism, not what it names)

**Dispatch error**: What a failed dispatch reports: the application error, boxed as a `BoxError`, wrapped with the tier,
the delivery's ID, kind, action and installation ID, the handler name and the registration site of the failing handler.
Says where, not why; why is its source, the boxed application error,
which a policy that wants its own type back downcasts (`source.downcast_ref::<AppError>()`).
A decode failure is reported at the handler that needed the decode, the `DecodeError` as the source;
the one failure no handler owns is the `EventMeta` decode, reported with no tier, handler or registration site,
and no action or installation ID.
The type is `DispatchError`, an `Error` whatever the handler's error was, so a dispatcher nests as a route of another.
The policy seam can inspect it before returning it to the receiver.
_Avoid_: Handler error (the application error inside it), failure
(prose for the event, not the type), `DispatchError<E>` (the former generic shape; the source is always the box)

**Failed-delivery event**: The one `tracing` event at ERROR the receiver emits when a handler fails, `handler failed`:
the delivery's identifying fields (delivery ID and event name from the headers, and action and installation ID when
the handler failed with a dispatch error whose decoded meta has them)
and the handler's error, boxed, as `error`, an error value whose text and chain of sources the subscriber renders
(`error=<where> error.sources=[<why>, ..]` under the `fmt` subscriber).
With the `tracing` feature, one event per failed delivery, error included.
`error` is the one field name recorded in two forms,
text alone on the receive span for a refusal and an error value here;
to every subscriber it is the error's text in both.
_Avoid_: Handler error event (the removed second event), log line
(a subscriber's rendering of it),
error event in lowercase (ambiguous with the `error` field;
"ERROR event" and "event at ERROR" name the level and are fine), `source` as a field
(the removed second field; the chain is under `error`)

**Outcome**: What one dispatch reports: whether the delivery was matched, and if not,
whether its kind was known to the route table.
Distinct from success: a matched delivery can fail, an unmatched one can succeed.
_Avoid_: Status (reserved for HTTP), result (the Rust type)

**Match**: A delivery matches when at least one routed handler is registered for its kind, or its kind and action.
Matching is decided by the route table, never by a handler; the always and fallback tiers do not match.
_Avoid_: Hit, handled (a matched delivery may still fail)

**EventMatcher**: The kinds and actions one dispatcher registration selects: a kind, several kinds, a kind with actions,
or kind/action pairs, expanding to slots of `(kind, Option<action>)`.
`on` takes any `IntoMatcher` for its handler's input: those shapes, which say their kinds and fit any input, or,
for a handler over a `Payload`, actions alone, an `Action`, an array of them or `AnyAction` for every action,
the kind coming from the payload type.
In prose, a matcher that says its kinds is _absolute_; one that takes the kind from the type is _relative_.
_Avoid_: Filter, selector, route (a route is what a matcher registers), wildcard
(for `AnyAction`; it selects every action of one kind, not every delivery)

**Kind**: The parsed identity of an event, as the `EventKind` enum.
The three wire vocabularies (`EventKind`, `Action`, `TargetType`) are built through `From<&str>`, `From<String>`,
or the const `from_static`, so recognized strings become named variants.
`Unknown` holds a vocabulary-specific opaque value
(`UnknownEventKind`, `UnknownAction`, or `UnknownTargetType`)
with a private `Cow<'static, str>` and is variant-level `#[non_exhaustive]`:
consumers match `Unknown { value, .. }` and read `value.as_str()` or display it,
but cannot construct it directly or edit its string.
Separate value types prevent moving a string unknown to one vocabulary into another that already recognizes it.
To change a name, construct the enum again through a normalizing conversion.
`from_static` borrows an unknown name and lets a `Payload::KIND` declare a kind the crate does not yet know;
upgrading to a version that knows it normalizes the same declaration to the named variant.
_Avoid_: Category, type (the Rust keyword and GitHub's overloaded "event type")

**Event name**: The raw `X-GitHub-Event` wire string before parsing.
A _name_ is unparsed; a _kind_ is parsed.

**Action**: The payload's top-level `action` value, GitHub's sub-classification of an event.
GitHub's own term, used verbatim.

**Delivery ID**: The `X-GitHub-Delivery` GUID identifying one delivery attempt; the consumer's idempotency key.
In prose, "delivery" names one attempt
("runs for every delivery", "fails the delivery"); it never names the envelope or any type.

**Target**: The resource the webhook is configured on, GitHub's _hook installation target_,
from the `X-GitHub-Hook-Installation-Target-Type` and `-ID` headers: `integration` for a GitHub App,
`repository` for a repository webhook, `organization` for an organization webhook, as the fields `target_type`
(a `TargetType`) and `target_id`, on `WebhookMeta` and `EventMeta` alike.
The one meta field pair read from headers GitHub does not always send.
The key a `VerifierSource` typically selects a secret by.
It names the webhook's resource, not the webhook: two webhooks on one repository share a target.
Held as two fields for now; one `Target` value, present only when both headers are, is a separate future change,
made on both metas at once.
Distinct from the `installation_target` event kind, which reports a change to a target.
_Avoid_: Hook target, installation (the App installation, `installation_id`, is a different thing), owner, scope

**WebhookMeta**: The values the receiver reads from a request's headers before the body: delivery ID, kind,
target type and target ID, but not the signature, which is parsed on its own into a `Signature`.
What a `VerifierSource` is given to select a verifier, what an `Envelope` carries beside the bytes,
and the header half of an `EventMeta`.
Unsigned when read, and still unsigned after verification, which authenticates the body alone: it selects a secret,
it never authorizes.
Named by the meta rule, the meta of the webhook request GitHub sends; `EventMeta` avoids "header"
because it also holds payload fields, and this type holds none.
_Avoid_: `HeaderMeta` (the former name), `WebhookMetadata`, `RequestMeta`
(the receiver reads no path or method),
`EventMeta::headers` as a nested group (the removed `Common`'s mistake), verified or signed meta

**Refusal**: A request answered before any handler ran,
as a `ReceiveError` and the status `ReceiveError::status` maps it to:
unauthorized (401) for a signature that is absent or does not match, or for headers no verifier is known
for (`UnknownTarget`), bad request (400) for a signature that is malformed, a missing required header,
a content type other than `application/json` or a body the transport could not read,
payload too large (413) for a body over the limit.
Payload bytes that are not valid JSON are not a refusal: the receiver reads nothing of the payload,
the envelope is built and handed to the handler.
Under a dispatcher they fail the delivery at dispatch, as the `EventMeta` decode, before any handler runs,
and the receiver answers 500, a handler failure; a receiver's handler over the envelope alone sees them as they are.
The receive span's `outcome` names the class and its `error` the refusal's text; no handler runs.
_Avoid_: Rejection, denial, failure (kept for a handler's), receive error as the concept (the type's name)

**View**: A consumer-defined serde type naming only the fields its handler reads,
decoded from the payload and indifferent to every other field GitHub sends or adds.
A view over one kind declares it with `#[derive(Payload)]` and is a `Payload`;
a view over fields several kinds share implements `FromEnvelope` itself with `Envelope::decode`.
Its decode reads the bytes on its own; the meta beside it, in `Event<P>`, is the dispatcher's, not re-read.
The crate's answer to one struct per kind.
_Avoid_: Model (octocrab's structs, the whole object), DTO, schema, projection (kept for the meta types)

**Wire format**: The one flat JSON object a serialized `Envelope` becomes,
the `WebhookMeta` fields at the top level beside `raw_payload` as base64, for a trusted internal hop to another
service, which reads it back through serde, nothing verified.
The payload meta is not on the wire: the consumer's dispatcher decodes it from `raw_payload`,
so forwarded meta cannot disagree with the bytes. Documents written by 0.3, which carried it,
read back with those fields ignored; consumers upgrade before producers.
Serialized by the crate, read by anything.
Follows crate versioning, with no separate version field.
Added optional fields are compatible; removed or renamed fields, newly required fields,
or changed encodings or meanings are breaking changes documented in release notes.
A producer and consumer crossing a breaking boundary migrate together or use a transport adapter.
_Avoid_: Serialization format (the mechanism), envelope format, transport format, message

**Authenticate**: The one path from an untrusted request to an envelope, `octoevents::authenticate`, over the verifier,
the headers and the body.
The signature header is parsed (`Missing`, `Malformed`) and _verified_ over the body (`Mismatch`),
the content type is checked, then the required headers are read into a `WebhookMeta` and the envelope is built from it
and the body.
A free function beside the receiver, which calls it after asking its `VerifierSource`,
so the receiver and a transport built on it authenticate alike.
Not a method on `Verifier`, which knows the secrets and the HMAC and nothing of headers,
and not a constructor on `Envelope`, which is data.
The content-type check lives here and not in a data constructor:
refusing a form-encoded body is what makes `raw_payload` the bytes that were signed.
_Avoid_: `Envelope::from_signed` (the removed constructor, which made an envelope look verified by its type),
verify as the name of this step (verify is the HMAC inside it)

**Verify**: The mechanism: HMAC comparison of `X-Hub-Signature-256` against the body, `Verifier::verify`,
over a `Signature` already parsed.
It decides `Mismatch` and nothing else: whether the header is there is decided from the headers (`Missing`),
and whether it is a signature by parsing it (`Malformed`), both before the verifier is asked.
"Authenticate" is acceptable in prose for the goal verification achieves; as a term it names the whole request step,
_authenticate_, of which verification is one part.
_Avoid_: Validate (collides with schema validation, despite GitHub's docs)

**Signature**: The `X-Hub-Signature-256` value parsed once, as the type `Signature`: the 32 MAC bytes, nothing else.
A header value becomes one through `TryFrom<&HeaderValue>`, the path `authenticate` and the receiver take,
`TryFrom<&[u8]>` for the bytes in another shape, or `str::parse`,
for a consumer parsing a string in a test or their own early-out, and that parse is the one origin of `Malformed`;
`Display` renders the header value back and `From<Signature> for HeaderValue` puts it on a request, `Debug` is redacted,
and it has no comparison, neither `PartialEq` nor `ConstantTimeEq`:
the MAC bytes are compared inside `Verifier::verify`, over every configured secret,
and that is the verification path the crate offers.
A constant-time `==` would be safe (`digest::CtOutput` has one) but would hand out a second path,
one secret and no verify span; comparing two rendered signatures as strings stays possible,
as it must for a printable value, and is a way around the path, not one the crate offers.
What `Verifier::sign` produces and `Verifier::verify` takes,
so the verifier is handed a settled format and can only mismatch.
In prose, "signature" alone names the value once the header is in context;
"signature header" names the wire string before parsing, as "event name" does for the kind.
_Avoid_: MAC or tag as the type (the bytes inside, not the parsed header value), digest
(a hash, not a MAC),
signature header as the type (the unparsed string), `HeaderValue` (the `http` type it arrives as and converts back into)

**Verifier**: The component owning the configured secrets and performing signature verification.
The secrets of one webhook, its rotation included;
a receiver serving several webhooks is given one per request by a `VerifierSource`.
Required to build a receiver, so a deployment without a secret cannot be expressed.
It also signs (`Verifier::sign`): the `X-Hub-Signature-256` value GitHub would send for a body under its first secret,
so a test of the receiving side can put a synthetic request through the receiver it built.
That is a test aid, not a sending-side feature: the crate sends nothing, and the sending side's vocabulary
(queueing, retries) stays out.
Lives with the secret and the signature error in the `signature` module, the one place the secret's bytes are read.
_Avoid_: Validator, authenticator, signer (a role the verifier plays for a test, not a component), signature verifier
(nothing else at the crate root is verified, so the qualifier adds length and no meaning),
keyring (secrets tried in turn is what a verifier already is; and they are not keys)

**VerifierSource**: What a receiver asks, per request and before verification,
for the `Verifier` of the request's `WebhookMeta`, typically by its target: one webhook URL serving several GitHub Apps,
each signing with its own secret.
Asynchronous, so the secrets can come from a secret manager.
A `Verifier` is one, answering itself for every request, so a single-secret receiver names no source.
Finding no verifier, for a missing target, an unknown one or a failed lookup, is the `UnknownTarget` refusal,
answered as a mismatch is.
Selecting a secret by an unsigned header is safe: a forged target selects a secret its sender does not know,
so verification fails.
Attributing a verified delivery to its target is sound only when the source chose the verifier by that target,
answering each target with its own secrets only, and no two targets share a secret, both the source's to ensure;
under a single `Verifier`, which ignores the target, the target stays a claim.
It runs for unauthenticated requests too, so a remote lookup per request is on the path of forged traffic.
_Avoid_: Keyring (secrets tried in turn, which is a `Verifier` with `also`), secret store or secret provider
(the source hands out verifiers, not secrets), resolver

**WebhookSecret**: The shared HMAC key configured on the GitHub webhook.
GitHub's own term, in full: the type is `WebhookSecret`, beside `WebhookReceiver`,
so the crate's name for the thing GitHub calls the webhook secret says which secret,
and so it does not collide with `secrecy::Secret` in a consumer's imports.
Never empty: an empty one is the unset-environment-variable failure mode, not a configuration,
and every constructor refuses it, `new` by panicking, for a deployment that reads its secret at startup,
and `str::parse`, `TryFrom<Vec<u8>>` and `TryFrom<&[u8]>` with a `WebhookSecretError`,
for one that reads it per request or as bytes, so the verifier has nothing left to check for emptiness.
Consumers supply a high-entropy secret; nonempty validation does not establish secret strength.
In prose, "secret" alone is fine once the webhook is in context.
_Avoid_: Token, `Key` or `SigningKey` as the type
(it is a secret to GitHub and to the crate; "HMAC key" in prose, for what the bytes are to the MAC, is fine),
`Secret` as the type (the former name; generic at the root and a live collision), signing secret
(Stripe's and Svix's term; the crate's is GitHub's)

**SignatureError**: Why a body did not authenticate: the `X-Hub-Signature-256` header was `Missing`, `Malformed`
(not `sha256=` and 64 hex digits), or a `Mismatch` under every configured secret.
Each variant has one origin: `Missing` is decided from the headers,
`Malformed` by parsing the header into a `Signature`, `Mismatch` by `Verifier::verify`, in that order,
so the verifier is handed a parsed value and has no format left to refuse.
Named for its subject, the signature, not for the operation: two of its three variants are decided before `verify` runs,
so an error named after verifying misdescribed most of itself.
The `Signature` variant of `ReceiveError`.
Distinct from `WebhookSecretError`, a configuration failure found before any delivery arrives.
_Avoid_: `VerifyError` (the former name), `MissingSignature` and `MalformedSignature`
(the former variants; the type already says signature),
authentication error (the goal, not the mechanism; also reads as GitHub App auth)

**Payload**: The JSON document GitHub sends, in two states
that two fields name: the _raw payload_ (`Envelope::raw_payload`), the exact bytes as they arrived,
undecoded and never re-encoded; and the payload decoded for one handler (`Event::payload`).
On the receiving path the raw payload is also the signed input; form encoding is refused so the two stay the same bytes.
As a type (`Payload`), one kind's decoded payload, declaring the kind it belongs to; octocrab's per-kind structs,
consumer-defined serde views and `Event<P>` over any of them are payloads;
octocrab's `WebhookEvent` and a view over several kinds are not.
_Avoid_: Body (reserved for the HTTP transport layer), raw unqualified
(says unprocessed without saying of what, and named a removed tier; "raw payload" is the term)

**Decode**: Turning an envelope into a handler's input, through `FromEnvelope`,
the one way to decode and spelled the same for every input, `P::from_envelope(&envelope, &meta)`,
given the `EventMeta` the caller decoded once with `EventMeta::decode`:
a serde `Payload` type checks the kind and then decodes the bytes,
`EventMeta` is a clone of the meta it is given and `Envelope` a clone of the envelope,
`Event<P>` pairs the meta with `P`'s decode,
octocrab's `WebhookEvent` decodes into octocrab's model,
and a consumer type implementing `FromEnvelope` itself decodes as it sees fit,
a view over several kinds with the kind-free `Envelope::decode`, the one decoding primitive on the envelope.
A decode failure is a `DecodeError` saying why
(a kind mismatch, a JSON error, or the input's own reason, `DecodeError::Input`,
a non-exhaustive variant built through `input` or `input_with_source`,
its optional source a `BoxError` with the same platform bounds as handler errors,
the one a consumer's impl returns for a failure that is neither) and fails the delivery at the position of the handler
that needed it.
Decoding the `EventMeta` itself is a decode too, the dispatcher's, once per delivery; its failure fails the delivery
before any handler runs.
_Avoid_: Parse (kept for the header-to-kind step), deserialize
(the serde mechanism, not the concept),
`decode_payload` and `decode_event` (removed inherent spellings of `from_envelope`)
