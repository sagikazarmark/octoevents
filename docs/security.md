# Security and authorization

[Documentation index](../README.md#documentation)

## Authentication boundary

A receiver requires a `Verifier` with at least one nonempty `WebhookSecret`.
It checks only `X-Hub-Signature-256`, never SHA-1. Supply a high-entropy secret;
nonempty validation does not establish strength. `WebhookSecret::new` panics
on an empty value at startup; use `str::parse::<WebhookSecret>` when an empty
value must be handled as an error, for example during per-request configuration.

Verification authenticates the exact payload bytes. Do not parse, normalize
or re-encode a request before verification. Form-encoded deliveries are
refused; configure the GitHub webhook to send `application/json`.

The delivery ID, event name and target headers are **not signed**. A valid
signature establishes neither the claimed event kind nor authorization for
an operation. The receiver records header-derived metadata as routing input,
not as an authenticated identity claim.

## Authorize the operation, not the route

For an operation limited to one repository, compare the authenticated
payload's numeric repository ID with an allowlist from trusted application
configuration. Fail closed if required payload data is absent. Names and
logins can change; use numeric IDs for identity.

A small view can require the fields a decision needs, but decoding that view
does not authenticate the header-derived kind. An attacker who has captured
a signed payload can present it with another event name or delivery ID.

For example, routing a handler under `installation.deleted` and reading an
authenticated `installation.id` is **not enough evidence to revoke access**.
Other signed documents can contain an installation ID and an action named
`deleted`. Before a destructive action, independently confirm the intended
state through the GitHub API or enforce an equivalent policy based on trusted
configuration and authenticated payload evidence. Handle an unavailable
confirmation service as a failure/deferred decision, not permission to proceed.

A useful authorization sequence for a repository-scoped mutation is:

1. Receive through the verifying receiver.
2. Require the operation's payload fields and stable resource IDs.
3. Compare those IDs with the configured tenant/resource allowlist.
4. Where the decision relies on event identity or current permissions, confirm
   the resource's state through an independently authenticated GitHub API call.
5. Perform an idempotent operation and record its result.

The crate has no universal authorization helper: permission to perform an
operation depends on your application and its configured GitHub credentials.
Example handlers print diagnostics rather than implementing such permissions.

## Replay and redelivery

GitHub signs no timestamp. Delivery-ID deduplication recognizes GitHub's
redelivery of the same ID, but does not stop a captured signed payload from
being submitted under a new ID. If stale input matters, reconcile it with
current authoritative state before acting. Rotation ends acceptance under a
removed secret, but is not a timestamp or ordering guarantee.

See [Durable receipt and recovery](recovery.md) for atomic deduplication,
partial execution and repeated side effects.

## Rotate a webhook secret

GitHub holds one secret per webhook; your verifier can temporarily accept two:

```rust
use octoevents::{Verifier, WebhookSecret};

let verifier = Verifier::new(WebhookSecret::new("new-development-secret"))
    .also(WebhookSecret::new("previous-development-secret"));
```

1. Generate a new high-entropy secret in your secret-management system.
2. Deploy all receivers accepting the new secret first and the previous one
   through `also`. Keep the previous configuration available for rollback.
3. Change the webhook's secret in GitHub to the new value and confirm a new
   delivery succeeds.
4. Wait for your explicitly bounded in-flight request and forwarding-buffer
   window to drain, then remove the previous secret from every receiver.
   Account for any transport that buffers original signed requests. If no
   finite drain bound is known, choose a cutoff and a recovery procedure for
   old requests rather than waiting indefinitely.
5. Test that a synthetic request under the new secret succeeds and one under
   the previous secret is refused. Recover any missed work from your durable
   store or through GitHub's redelivery procedure.

Every configured secret is evaluated using constant-time MAC comparison;
verification does not disclose which matched. The verify span's
`secret_count` and `outcome` therefore cannot prove that old traffic has
drained. `Verifier::sign` is a receiving-side test aid and uses the first
secret; it does not expose which secret verified a real request.

## Resource and response boundaries

- Missing or malformed signature headers are refused before `receive` reads
  the body. With `receive_bytes`, the caller has already read it.
- The body limit bounds accumulated payload length. Buffer capacity and
  transport-owned frames can exceed it; it is not a process-memory ceiling.
  Bound body-read time and concurrent requests in your HTTP transport too.
- A signed body need not be valid JSON. The metadata probe is best-effort;
  a handler that decodes invalid bytes fails at its decode. Authentication
  and JSON/schema acceptance are separate decisions.
- A verified `ping` is answered 204 before handlers by default;
  `handle_ping(true)` passes it through. The event name remains unsigned.
- HTTP responses contain only a status. The crate never records secrets,
  signatures or computed MACs, but application errors can contain whatever
  the application supplies. See [Observability](observability.md).

## Trusted internal forwarding

Serializing an `Envelope` uses the documented
[wire format](https://docs.rs/octoevents/latest/octoevents/struct.Envelope.html#wire-format):
flat metadata beside base64-encoded exact payload bytes. Deserializing it
**neither verifies a signature nor probes the metadata again**.

Authenticate and authorize the forwarding producer at the receiving service
(for example through authenticated service-to-service transport), and restrict
ingress to that producer. TLS encryption by itself does not authenticate a
caller to the application. Apply size limits to the encoded document as well
as the payload; base64 adds roughly one third to payload size.

Preserved payload bytes could be verified against a separately retained
GitHub signature, but the wire format does not carry that signature and
reverification would still not authenticate the forwarded metadata. The hop
therefore trusts the producer's metadata and acceptance policy. Never expose
serde deserialization as the public GitHub webhook endpoint.

The [Worker forwarder](../examples/worker/README.md) illustrates the outbound
POST, not ingress authentication at the destination. Configure the destination's
service identity/access controls before using it beyond local development.
Coordinate producer and consumer upgrades across breaking wire-format changes.
