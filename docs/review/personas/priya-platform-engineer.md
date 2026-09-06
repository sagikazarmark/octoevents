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

## Baseline (2026-09-05)

Compile iterations for the whole application: 1.

- The raw tier ended **empty**: persist plus dedup-skip cannot be expressed in any tier (no tier can succeed and stop), so persistence, dedup, dead-lettering and error logging all moved into one hand-written wrapper around `dispatch`. The docs recommended the raw tier for persistence and the wrapper for dedup without connecting the two.
- No meta-only routing by kind and action. With octocrab, `on` forced a full `WebhookEvent` decode; without octocrab, `on` did not exist. The workaround (an empty serde view) parsed the whole document to read nothing and failed the delivery on a non-object body.
- The dispatch span labelled an `always`-tier failure on an unrouted kind `fallback_error` with no fallback registered.
- With `tracing` on, the crate emitted nothing about the handler error itself: only an outcome label and a status.
- octocrab's `installation` payload struct omits the top-level `installation` object, so a typed handler could not see the tenant's login; forced onto the octocrab event path.
- The dispatch span recorded neither action nor installation ID; `delivery_id` was recorded quoted on one span and unquoted on another; `outcome` was a string on one span and a number on another.
- `From<Infallible>` ritual; three `From` impls total (one mandated, one ritual, one real).
- A one-argument closure passed to `on` produced an arity error rather than a flavour hint.
- The adapter types and the flavour conversion were never called; the event-handler flavour was judged "the payload handler in all but name".
