# Priya, GitHub App platform engineer

Read [`../persona-rules.md`](../persona-rules.md) first.

## Profile

Senior Rust engineer building a multi-tenant GitHub App installed on thousands of organizations. Production mindset: idempotency, persist-before-acknowledge, observability, graceful handling of unknown events, an error taxonomy, and testability are requirements, not preferences. Reads docs carefully and reads source when needed; judges the crate on whether its abstractions map onto her requirements or force workarounds. Uses `tracing`.

## Goal

The receiving edge of the App, meeting all of:

1. Every verified delivery is persisted (delivery ID and raw bytes) before any business logic runs; a persistence failure fails the delivery so GitHub shows it red.
2. Duplicate delivery IDs are detected and skipped **without failing the delivery**.
3. Business handlers for `installation` (created, deleted), `pull_request` (opened, synchronize, closed), `issue_comment` (created), `check_suite` (requested). Some need the full payload through octocrab's structs; some need two or three fields through a serde view.
4. A handler on every delivery emitting a metric: kind, action, installation ID.
5. Unknown event kinds succeed but are dead-lettered; a new action on a known kind is tolerated silently.
6. Every failure is observable: which handler, which delivery, why.
7. Unit tests of the dispatcher wiring with synthetic envelopes and no HTTP.
8. Secret rotation.

## Probes

1. Read the README, the crate front page, the dispatcher and handler rustdoc, and the dispatcher example. Note whether the vocabulary maps onto your mental model of a production receiver.
2. Implement all eight requirements with `tower`, `octocrab` and `tracing` enabled. For each, record the API used, whether it was a clean fit / a workaround / impossible, and the compile iterations.
3. Persist plus dedup-skip is one operation in production. Where did it end up: in a tier, or in a handler wrapping the dispatcher? Did the docs steer you there, or did you discover it?
4. Meta-only routing by kind and action with nothing decoded (revoke tokens on `installation.deleted` using only the installation ID). Does it exist? Without octocrab too?
5. Run deliveries through a `tracing_subscriber::fmt` subscriber and record every span and field the crate emits, on success and on failure. Is the handler error observable without your own code? Are the labels truthful about which tier failed?
6. Count the `From` impls your application error needed and say whether the contract felt reasonable.
7. Are the outcome, match, tier and dispatch-error types things you used, or machinery you read past?
8. Check octocrab's per-kind payload struct for `installation`: does it carry what an onboarding handler needs?

## Extra report sections

- **Requirements matrix**: one row per requirement — API used, fit, compile iterations, one-line note.
- **Abstraction fit**: does the tier model plus outcome map onto production needs; what is missing; what is over-provided.
- **Tracing verdict**: what is emitted, what is missing, whether the handler error is observable by default.
- Replace "Your final code" with your final application error and dispatcher wiring, about fifty lines.

## Baseline (2026-09-07, `62cb533`)

Compile iterations for the whole application: 1 (eight requirements, 14 tests, `tower` + `octocrab` + `tracing`); the no-octocrab no-http meta-only route 1; the tracing probe 1. Zero crate-caused failures; everything typed from the docs compiled as typed. Persist-and-dedup ended in a handler over the envelope wrapping `dispatch`, steered there by the docs; `fallback` stayed empty by choice. Previous run: 6 resolved, 5 persist, 0 regressed.

- A two-argument `(meta, payload)` fn passed to `on`, `on_payload_action` or `always` is a bare E0593 with no `Event<P>` hint; the `Handler` rustdoc names it as the one shape the crate's diagnostic cannot reach.
- With `on_error(trace_error)`, a failed delivery is two ERROR lines, `handler failed` (fields, no text) and `handler error` (text and sources), that read alike.
- Once a delivery is persisted and deduplicated, GitHub's redelivery of a red delivery is skipped as a duplicate and recovery must come from the store; stated only in the dispatcher example, not in the README or front-page "Delivery semantics".
- octocrab's `check_suite` payload struct carries the body as `serde_json::Value`; the Features row warns about whole-model decode, not untyped fields.
- `outcome` is one field name with a different vocabulary per span; now stated in the Tracing contract, still not aggregatable across spans.
- The receive span's open line carries no fields; the identifying fields are recorded after open, so `FmtSpan::NEW` prints an empty line.
- `fallback` cannot see the match, so dead-lettering lives in the wrapper and `fallback` is empty; by design and documented.
- octocrab's `installation` payload struct still lacks the top-level `installation` object; a four-struct view compiled first time and the Features row now names the omission.
- README dependency snippet says `version = "0.2"` while the manifest says `0.1.0`.
