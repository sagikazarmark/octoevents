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

## Baseline (2026-09-05)

Unique crates: 30 no-default / 35 default / 319 with octocrab. No-default builds clean for `wasm32` in about five seconds cold; the crate added about 69 KB of wasm over a serde-only baseline, the dispatcher about 16 KB more. Shapes: plain handler 26 lines, adapted typed handler 47, dispatcher 43.

- The sans-I/O path silently lacked the receiver's ping short-circuit, body limit and header-only pre-rejection; a `ping` was forwarded to the queue. Undocumented.
- No public header-name constants; header-name case handling undocumented.
- `Bytes` required but not re-exported.
- The dispatcher required a decode-error conversion and an `Infallible` conversion on the application error even for one event.
- No combinator to sequence two envelope handlers; hand-written wrapper needed.
- Handing a typed handler straight to the receiver answered 500 for every other kind; a forward-everything receiver needed a wrapper that swallowed the kind mismatch.
- README had no sans-I/O or Lambda section; the one pointer was in the response-status rustdoc.
- Wire format had no example document, no field table, explicit nulls for absent fields.
- The worker example's comment claimed a `default-features = false` build excludes octocrab, which was never a default.
- The platform-conditional bounds were invisible; a `std::sync::Mutex` handler compiled natively and for `wasm32` unchanged.
- The payload-declaring macro expands to a three-line impl; judged marginal but fine, kept for hygiene and its diagnostic.
