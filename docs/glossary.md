# Reader glossary

[Documentation index](../README.md#documentation) · [API concepts](https://docs.rs/octoevents/0.3.0/octoevents/#concepts)

| Term | Meaning |
| --- | --- |
| Envelope | Exact payload bytes and `EventMeta`. `from_signed` verifies receipt; `new` is an unverified test constructor; serde reads a trusted forwarded envelope |
| EventMeta | Delivery ID, kind, action, installation, repository, organization, sender and target metadata |
| Event name / kind | The raw `X-GitHub-Event` string / its parsed `EventKind`. Both originate in an unsigned header |
| Action | The payload's top-level `action`, when present |
| Delivery ID | GitHub's `X-GitHub-Delivery` value; useful for redelivery deduplication, not authentication |
| Installation / target | An App installation in payload data / the resource the webhook is configured on, from target headers |
| Payload / view | The JSON GitHub sends / a consumer's serde type naming only the fields it needs. `Payload` declares one kind |
| Event<P> | Metadata beside a decoded payload, as `meta` and `payload` |
| Probe / decode | Best-effort metadata read at envelope construction / fallible conversion to one handler's input |
| Receiver | `WebhookReceiver`: authenticates, bounds and dispatches one request, leaving paths and methods to the caller |
| Handler | Consumer code accepting one `FromEnvelope` input and returning `Result<(), E>` |
| Dispatcher | A handler over envelopes that selects other handlers by kind and action |
| Tier | Always, routed handlers, then fallback when no route matched. Execution is sequential within a dispatch |
| Route table / match | Registrations from `on` / whether the table has a handler for this kind or kind/action |
| Outcome | Match classification plus the result of dispatch; unmatched can succeed and matched can fail |
| Refusal | A request answered before any handler ran, such as an invalid signature or oversized body |
| Dispatch error | The failing handler's error, wrapped with delivery, tier, handler name and registration site |
| Policy seam | The handler over an envelope wrapping `dispatch`, where persistence, deduplication and outcome policy live |
| Redelivery | Another GitHub attempt with the same delivery ID, requested by an operator or automation; GitHub does not do it automatically |
| Wire format | The flat JSON representation of an envelope for a trusted internal hop; deserialization authenticates nothing |

For contributor terminology and design rationale, see [the domain language](../CONTEXT.md).
