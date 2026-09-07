# octoevents

Receiving-side GitHub webhook handling: turning an untrusted HTTP request into
a verified envelope and routing it to consumer handlers. The sending side
(queueing, retries, redelivery) is a separate project (`octodelivery`) and its
vocabulary is deliberately kept out of this one.

## Language

**Envelope**:
The verified unit of receipt: exact payload bytes plus the routing metadata
extracted from headers and a best-effort payload probe. Composed of an
`EventMeta` and the raw bytes.
_Avoid_: Delivery (reserved for the outbound `octodelivery` project), event (the decoded unit, `Event<P>`, is the envelope decoded for one handler), message

**EventMeta**:
The envelope's routing metadata without the payload bytes: delivery ID, kind,
action, installation ID, repository, organization, sender, target. An input
in its own right, for a handler routed by kind and action that reads no
payload; the `meta` half of `Event<P>`; and what the error observer receives
alongside the handler's error.
_Avoid_: Common (the former nested group; its name carried no meaning), header (it also holds probed payload fields), delivery (reserved for `octodelivery`), receipt (reads as acknowledgement, and sits too close to Receiver), context (implies ambient services; this is plain data), routing (delivery ID and sender are not routing)

**Receiver**:
The component that authenticates, bounds, and dispatches one HTTP request,
owning no routing of paths or methods.
_Avoid_: Service (names the optional Tower impl, not the concept), endpoint, listener

**Handler**:
Consumer-owned code that handles one verified delivery, received as one
input: an `async fn` item, a struct implementing `Handler<I>`, or a closure.
Handlers *handle*; the receiver *receives*. One trait, named by what it
receives: the input type `I` is any `FromEnvelope`, and it says what the
handler gets and what is decoded for it: the `Envelope` (bytes included), the
`EventMeta` alone, a `Payload` view alone, or `Event<P>` for the meta beside
the payload. The receiver and the always and fallback tiers take a handler
over the envelope; a routed handler is over any input. In prose, "a handler
over `Envelope`", "a handler over `Event<P>`".
_Avoid_: Callback, subscriber, webhook handler and event handler (the former two flavours; now one trait and an input type), typed handler, payload handler (survives only in `on_payload` and `on_payload_action`, which register a handler whose payload type fixes the kind), raw handler (raw named a removed tier), meta handler (removed; a handler over `EventMeta` receives only the metadata)

**Event**:
`Event<P>`: the envelope decoded for one handler, the `EventMeta` beside the
payload decoded as `P`, as the named fields `meta` and `payload`. Distinct
from Envelope, whose payload is bytes. Meta and payload together is one
input, destructured in the parameter: `Event { meta, payload }:
Event<IssueOpened>`. When `P` is a `Payload`, so is `Event<P>`, of the same
kind, so `on_payload` takes a handler over either.
_Avoid_: Decoded envelope, typed envelope, context (implies ambient services)

**Dispatcher**:
A handler that routes envelopes to other handlers by kind and action, in
tiers: the *always* tier, the matched routes, then the *fallback* chain.
Produces an *outcome*. Part of the crate's core with every registration
method; only octocrab's input types need the `octocrab` feature. A policy the
tiers cannot express (skip a duplicate, dead-letter an unmatched delivery)
lives in a handler over the envelope wrapping `dispatch`, the *policy seam*.
_Avoid_: Router (implies path/method routing, which stays with the caller)

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
`on_payload`, `on_payload_action`, `fallback`), captured at compile time
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
What a failed dispatch reports: the application error wrapped with the tier,
the delivery's ID, kind and action, the handler name and the registration
site of the failing handler. Says where, not why; why is its source, the
application error. A decode failure is reported at the handler that needed
the decode. What the error observer receives when a dispatcher is the
receiver's handler.
_Avoid_: Handler error (the application error inside it), failure (prose for the event, not the type)

**Error observer**:
The callback registered with `on_error` on the receiver builder, called with
the event meta and a reference to the handler's error after a handler fails
and before the 500 is answered. Synchronous, with no bound on the error type,
and never called for a receive failure or a short-circuited ping. Not how the
error's text reaches `tracing`; that is the failed-delivery event's setting.
_Avoid_: Error handler (it handles nothing; the response is unchanged), hook, middleware, trace_error (the removed observer that emitted a second event)

**Failed-delivery event**:
The one `tracing` event at ERROR the receiver emits when a handler fails,
`handler failed`: the event meta's identifying fields and the status, and,
when the receiver builder was asked with `trace_errors` (an `Error`) or
`trace_boxed_errors` (a `BoxedError`: an error behind a pointer, or a dispatch
error over one), the error's text as `error` and its source as `source`, the
chain beneath rendered by the subscriber. One event whether or not the text
is on it and whether or not an observer is registered.
_Avoid_: Handler error event (the removed second event), log line (a subscriber's rendering of it), error event in lowercase (ambiguous with the `error` field; "ERROR event" and "event at ERROR" name the level and are fine)

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
`(kind, Option<action>)`.
_Avoid_: Filter, selector, route (a route is what a matcher registers)

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

**Verify**:
The mechanism: HMAC comparison of `X-Hub-Signature-256` against the body.
"Authenticate" is acceptable in prose for the goal verification achieves.
_Avoid_: Validate (collides with schema validation, despite GitHub's docs)

**Verifier**:
The component owning the configured secrets and performing signature
verification. Required to build a receiver, so a deployment without a secret
cannot be expressed.
_Avoid_: Validator, authenticator

**Secret**:
The shared HMAC key configured on the GitHub webhook. GitHub's own term.
_Avoid_: Token, key

**Payload**:
The JSON document GitHub sends: the envelope's raw bytes, viewed as content
rather than as signed input. As a type (`Payload`), one kind's decoded
payload, declaring the kind it belongs to; octocrab's per-kind structs,
consumer-defined serde views and `Event<P>` over any of them are payloads;
octocrab's `WebhookEvent` and a view over several kinds are not.
_Avoid_: Body (reserved for the HTTP transport layer)

**Decode**:
Turning an envelope into a handler's input, through `FromEnvelope`: a serde
`Payload` type checks the kind and then decodes the bytes
(`Envelope::decode_payload`), `EventMeta` decodes nothing and `Envelope` is
a clone, `Event<P>` pairs the meta with `P`'s decode, octocrab's
`WebhookEvent` decodes into octocrab's model (`Envelope::decode_event`), and a
consumer type implementing `FromEnvelope` itself decodes as it sees fit, a
view over several kinds with the kind-free `Envelope::decode`. A decode failure
is a `DecodeError` saying why (a kind mismatch, a JSON error, or the input's
own reason, `DecodeError::Input`, the one a consumer's impl returns for a
failure that is neither) and fails the delivery at the position of the handler
that needed it.
_Avoid_: Parse (kept for the header-to-kind and probe steps), deserialize (the serde mechanism, not the concept)
