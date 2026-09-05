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
action, installation ID, repository, organization, sender, target. What typed
handlers receive alongside a decoded payload.
_Avoid_: Common (the former nested group; its name carried no meaning), header (it also holds probed payload fields), delivery (reserved for `octodelivery`), receipt (reads as acknowledgement, and sits too close to Receiver), context (implies ambient services; this is plain data), routing (delivery ID and sender are not routing)

**Receiver**:
The component that authenticates, bounds, and dispatches one HTTP request,
owning no routing of paths or methods.
_Avoid_: Service (names the optional Tower impl, not the concept), endpoint, listener

**Handler**:
Consumer-owned code that handles one verified delivery. Handlers *handle*;
the receiver *receives*. Four flavours are distinguished by what they
receive: a *webhook handler* receives the envelope, a *meta handler* receives
only the metadata, an *event handler* receives the metadata plus octocrab's
decoded event, a *payload handler* receives the metadata plus one kind's
decoded payload. "Handler" alone means any of them. Event and payload
handlers are the *typed* handlers: the ones that need a decode.
_Avoid_: Callback, subscriber

**Webhook handler**:
A handler that receives the verified envelope, raw bytes included. The only
flavour the receiver accepts, and the only flavour the raw tier accepts;
other flavours reach the receiver through an explicit
`into_webhook_handler()` conversion.
_Avoid_: Raw handler (raw names the tier and the bytes, not the flavour)

**Meta handler**:
A handler that receives only the `EventMeta`: no bytes, no decode, so it can
run for a payload nothing can decode. What the always and fallback tiers
accept.
_Avoid_: Metadata handler, header handler (meta also holds probed payload fields)

**Event handler**:
A handler that receives the `EventMeta` and octocrab's decoded `WebhookEvent`
for any kind. For logic that spans kinds.
_Avoid_: WebhookEventHandler (collides with "webhook handler")

**Payload handler**:
A handler that receives the `EventMeta` and one kind's decoded payload. Bound
to that kind by its payload type, so registering it needs no matcher and
cannot disagree with the type.
_Avoid_: Typed handler (event handlers are typed too)

**Dispatcher**:
A handler that routes envelopes to other handlers by kind and action, in
tiers: the *raw* tier, the *always* tier, the matched routes, then the
*fallback* chain. Produces an *outcome*. Part of the crate's core: only
registering an event handler needs the `octocrab` feature.
_Avoid_: Router (implies path/method routing, which stays with the caller)

**Raw**:
The dispatcher tier that runs first for every delivery and receives the
envelope, bytes included. Where persist-before-route lives. Its failure fails
the delivery; it never counts as a match.
_Avoid_: Pre-tier, before hook

**Always**:
The dispatcher tier that runs for every delivery after the raw tier and
before routing, receiving only the metadata. Its failure fails the delivery;
it never counts as a match, so a strict fallback still rejects kinds nothing
else handles.
_Avoid_: Global handler, middleware

**Fallback**:
The dispatcher chain that runs only when no routed handler matched,
receiving only the metadata. Empty by default, so unmatched deliveries
succeed.

**Outcome**:
What one dispatch reports: whether the delivery was matched, and if not,
whether its kind was known to the route table. Distinct from success: a
matched delivery can fail, an unmatched one can succeed.
_Avoid_: Status (reserved for HTTP), result (the Rust type)

**Match**:
A delivery matches when at least one routed handler is registered for its
kind, or its kind and action. Matching is decided by the route table, never
by a handler; the raw and always tiers do not match.
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
Turning an envelope's payload bytes into a typed handler's input: octocrab's
`WebhookEvent` for an event handler, a `Payload` type for a payload handler.
A decode failure fails the delivery at the position of the handler that
needed it.
_Avoid_: Parse (kept for the header-to-kind and probe steps), deserialize (the serde mechanism, not the concept)
