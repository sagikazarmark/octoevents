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
_Avoid_: Delivery (reserved for the outbound `octodelivery` project), event, message

**EventMeta**:
The envelope's routing metadata without the payload bytes: delivery ID, kind,
action, installation ID, repository, organization, sender, target. What event
handlers receive alongside their decoded input, and what the error observer
receives alongside the handler's error.
_Avoid_: Common (the former nested group; its name carried no meaning), header (it also holds probed payload fields), delivery (reserved for `octodelivery`), receipt (reads as acknowledgement, and sits too close to Receiver), context (implies ambient services; this is plain data), routing (delivery ID and sender are not routing)

**Receiver**:
The component that authenticates, bounds, and dispatches one HTTP request,
owning no routing of paths or methods.
_Avoid_: Service (names the optional Tower impl, not the concept), endpoint, listener

**Handler**:
Consumer-owned code that handles one verified delivery: an `async fn` item,
a struct implementing the trait, or a closure. Handlers *handle*; the
receiver *receives*. Two flavours are distinguished by what they receive: a
*webhook handler* receives the envelope, raw bytes included, and is what the
receiver and the always and fallback tiers accept; an *event handler*
receives the metadata plus the envelope decoded as its input type. "Handler"
alone means either. The event handler is the *typed* flavour: the one that
needs a decode.
_Avoid_: Callback, subscriber, raw handler (raw named a removed tier), meta handler (removed; an event handler over `()` receives only the metadata)

**Event handler**:
A handler that receives the `EventMeta` and the envelope decoded as a type
implementing `FromEnvelope`: a `Payload` view over one kind, bound to that
kind by its type so registering it needs no matcher and cannot disagree with
the type; `()` for a handler routed by kind and action that decodes nothing;
octocrab's `WebhookEvent` for logic over octocrab's model; or a consumer view
over fields several kinds share.
_Avoid_: Payload handler (the former name; survives only in `on_payload` and `on_payload_action`, which register an event handler whose payload type fixes the kind), typed handler, WebhookEventHandler (collides with "webhook handler")

**Dispatcher**:
A handler that routes envelopes to other handlers by kind and action, in
tiers: the *always* tier, the matched routes, then the *fallback* chain.
Produces an *outcome*. Part of the crate's core with every registration
method; only octocrab's input types need the `octocrab` feature. A policy the
tiers cannot express (skip a duplicate, dead-letter an unmatched delivery)
lives in a webhook handler wrapping `dispatch`, the *policy seam*.
_Avoid_: Router (implies path/method routing, which stays with the caller)

**Always**:
The dispatcher tier that runs first, for every delivery, before routing,
receiving the envelope, bytes included. Its failure fails the delivery; it
never counts as a match, so a strict fallback still rejects kinds nothing
else handles. It can continue or fail but never skip.
_Avoid_: Global handler, middleware, raw (the removed tier that once ran before it)

**Fallback**:
The dispatcher chain that runs only when no routed handler matched,
receiving the envelope as the always tier does. Empty by default, so
unmatched deliveries succeed.

**Tier**:
One of the three steps a dispatcher runs a delivery through, in order:
always, route (the matched routes, action-specific then kind-wide), fallback.
A *dispatch error* names the tier its failing handler ran in.
_Avoid_: Stage, phase (kept for decode versus handle inside one handler), layer (middleware vocabulary)

**Registration site**:
The source location of the call that registered a handler (`always`, `on`,
`on_payload`, `on_payload_action`, `fallback`), captured at compile time
through `#[track_caller]`. What a dispatch error points an operator at.
_Avoid_: Call site (ambiguous with the handler's own calls), origin, registered at (reads as a time in code; fine in prose)

**Dispatch error**:
What a failed dispatch reports: the application error wrapped with the tier,
the delivery's ID, kind and action, and the registration site of the failing
handler. Says where, not why; why is its source, the application error. A
decode failure is reported at the handler that needed the decode. What the
error observer receives when a dispatcher is the receiver's handler.
_Avoid_: Handler error (the application error inside it), failure (prose for the event, not the type)

**Error observer**:
The callback registered with `on_error` on the receiver builder, called with
the event meta and a reference to the handler's error after a handler fails
and before the 500 is answered. Synchronous, with no bound on the error type,
and never called for a receive failure or a short-circuited ping.
_Avoid_: Error handler (it handles nothing; the response is unchanged), hook, middleware

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
payload, declaring the kind it belongs to; octocrab's per-kind structs and
consumer-defined serde views are payloads, octocrab's `WebhookEvent` is not.
_Avoid_: Body (reserved for the HTTP transport layer)

**Decode**:
Turning an envelope into an event handler's input, through `FromEnvelope`:
a `Payload` type checks the kind and then decodes the bytes
(`Envelope::decode_payload`), `()` decodes nothing, octocrab's `WebhookEvent`
decodes into octocrab's model (`Envelope::decode_event`), and a consumer view
over several kinds decodes as it sees fit (`Envelope::decode`). A decode
failure fails the delivery at the position of the handler that needed it.
_Avoid_: Parse (kept for the header-to-kind and probe steps), deserialize (the serde mechanism, not the concept)
