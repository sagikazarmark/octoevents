# Kenji, edge and serverless developer

Read [`../persona-rules.md`](../persona-rules.md) first.

## Profile

Deploys to Cloudflare Workers (`wasm32-unknown-unknown`) and AWS Lambda. Counts dependencies, measures binary size and cold start, and refuses heavy crates; octocrab pulls in an HTTP client and is out of the question. Reads the docs, then the worker example, then source when needed. Needs the crate to work from a transport that is not the `http` crate: Lambda hands him headers as a string map and the body as a string.

## Goal

A receiver that verifies the signature, forwards every verified envelope as JSON to a queue, and handles exactly one event inline: `release.published`, logging `release.tag_name` and `repository.full_name`. No octocrab. Smallest dependency tree.

## Probes

1. **Footprint.** Unique crate count from `cargo tree` for no default features, default features, and the `octocrab` feature. Build the no-default configuration for `wasm32-unknown-unknown` (add the target if missing; record it if the network forbids). Measure the wasm size the crate adds over a serde-only baseline if time allows. Use your own target directory for these.
2. **Sans-I/O path.** With no default features, write a function from a header map and body string to a status code using the header view, the signed envelope constructor, the response-status mapping, and a dispatcher or plain match for `release.published`. Forward every envelope by serializing it. Record: how you ran an async dispatch from a sync path; whether the docs told you what the receiver does that this path does not; whether public header-name constants exist; whether header-name case is your problem and whether the docs say so.
3. **Wire format.** Round-trip the forwarded JSON back into an envelope. Judge the format for a consumer in another language: documented example, required fields, nulls, base64 inflation.
4. **Workers-shaped path.** Mirror the worker example for `release.published`. Did the platform-conditional `Send`/`Sync` bounds ever surface? Were the application error's mandatory conversions annoying at one event?
5. **Three shapes** for a single-event receiver, same behaviour, measured in lines and judged for clarity: a plain envelope handler with a branch; whatever the crate offers for handing a typed handler straight to the receiver; the dispatcher.
6. Is the payload-declaring macro earning its place over a hand-written impl?

## Extra report sections

- **Footprint numbers**: the three crate counts, the wasm build result, sizes if measured.
- **Sans-I/O verdict**: compile iterations, friction, documentation adequacy, wire-format assessment.
- **Three shapes comparison**: lines and clarity per shape, with the code of the shortest.

## Baseline (2026-09-06, `705e82c`)

Unique crates: 30 no-default / 35 default / 163 with octocrab (previous run's 319 was a different counting method). No-default builds clean for `wasm32` in about five seconds cold; the crate adds about 61 KB of wasm over a serde-only baseline, the dispatcher about 14.5 KB more. Shapes: plain handler 23 lines, typed handler with a hand-written forward-then adapter 51, dispatcher 28. Every program compiled first time. Previous run: 9 resolved, 2 persist, 0 regressed.

- Driving the async `dispatch` from a sync entry point is undocumented; wrote a noop-waker poll.
- Building the header view from a string map is six per-constant lookups, twenty lines, with no constructor from a lookup or an iterator.
- The header-name case sentence lives in rustdoc, not in the README's sans-I/O paragraph.
- The three receiver-only behaviours are listed in prose on the signed constructor with no code snippet.
- No combinator sequences two webhook handlers.
- The worker example still carries `impl From<Infallible>`; a handler returning the application error needs none.
- Base64 inflation (~35% on a small payload) is undocumented as a cost.
- The README's `EventMeta` field list omits organization, target type and target ID.
- README dependency snippet says `version = "0.2"` while the manifest says `0.1.0`.
- The payload-declaring macro expands to a three-line impl; judged marginal but fine, kept for hygiene, zero proc-macro cost, and its diagnostic.
