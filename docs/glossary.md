# Glossary

[Documentation index](../README.md#documentation) ·
[API concepts](https://docs.rs/octoevents/latest/octoevents/#concepts)

| Term | Meaning |
| --- | --- |
| Envelope | Exact payload bytes and the `WebhookMeta`, as data with no verification claim; building one reads nothing of the payload. `new` builds one from a `WebhookMeta` and the bytes; serde reads a trusted forwarded envelope |
| Authenticate | `authenticate`: the one path from an untrusted request to an envelope. Verifies the signature, checks the content type, reads the headers, then builds the envelope; the receiver is built on it |
| EventMeta | Delivery ID, kind, action, installation, repository, organization, sender and target metadata; the dispatcher decodes it once per delivery with `EventMeta::decode` |
| Event name / kind | The raw `X-GitHub-Event` string / its parsed `EventKind`. Both originate in an unsigned header |
| Action | The payload's top-level `action`, when present |
| Delivery ID | GitHub's `X-GitHub-Delivery` value; useful for redelivery deduplication, not authentication |
| Installation / target | An App installation in payload data / the resource the webhook is configured on (type and ID), from target headers |
| WebhookMeta | Delivery ID, kind and target, read from the unsigned headers of the webhook request before the body; what an envelope carries |
| Verifier source | `VerifierSource`: chooses the `Verifier` per request from the `WebhookMeta`, for several GitHub Apps at one URL |
| Payload / view | The JSON GitHub sends / a consumer's serde type naming only the fields it needs. `Payload` declares one kind |
| Event | `Event<P>`: metadata beside a decoded payload, as `meta` and `payload` |
| Decode | Fallible conversion of the envelope, given its `EventMeta`, to one handler's input; the dispatcher's decode of the meta itself fails the delivery before any handler |
| Receiver | `WebhookReceiver`: authenticates, bounds and dispatches one request, leaving paths and methods to the caller |
| Handler | Consumer code accepting one `FromEnvelope` input and returning `Result<(), E>` |
| Dispatcher | A handler over envelopes that selects other handlers by kind and action |
| Tier | Always, routed handlers, then fallback when no route matched. Execution is sequential within a dispatch |
| Route table / match | Registrations from `on` / whether the table has a handler for this kind or kind/action |
| Outcome | Match classification plus the result of dispatch; unmatched can succeed and matched can fail |
| Refusal | A request answered before any handler ran, such as an invalid signature or oversized body |
| Dispatch error | The failing handler's error, wrapped with delivery, tier, handler name and registration site; or the meta's decode error, with no handler |
| Policy seam | The handler over an envelope wrapping `dispatch`, where persistence, deduplication and outcome policy live |
| Redelivery | Another GitHub attempt with the same delivery ID, requested by an operator or automation; GitHub does not do it automatically |
| Wire format | The flat JSON representation of an envelope for a trusted internal hop; deserialization authenticates nothing |
