# octoevents

Receiving-side GitHub webhook handling: turning an untrusted HTTP request into
a verified envelope and routing it to consumer handlers. The sending side
(queueing, retries, redelivery) is a separate project (`octodelivery`) and its
vocabulary is deliberately kept out of this one.

## Language

**Envelope**:
The verified unit of receipt: exact payload bytes plus the routing metadata
extracted from headers and a best-effort payload probe.
_Avoid_: Delivery (reserved for the outbound `octodelivery` project), event, message

**Receiver**:
The component that authenticates, bounds, and dispatches one HTTP request,
owning no routing of paths or methods.
_Avoid_: Service (names the optional Tower impl, not the concept), endpoint, listener

**Handler**:
Consumer-owned code that handles one verified envelope. Handlers *handle*;
the receiver *receives*.
_Avoid_: Callback, subscriber

**Dispatcher**:
A handler that routes envelopes to other handlers by kind and action.
_Avoid_: Router (implies path/method routing, which stays with the caller)

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
idempotency key. The only place "delivery" appears in this crate's language.

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
rather than as signed input.
_Avoid_: Body (reserved for the HTTP transport layer)
