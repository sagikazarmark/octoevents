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

## Baseline (2026-09-07, `62cb533`)

Unique crates: 30 no-default / 35 default / 163 with octocrab, unchanged. No-default builds clean for `wasm32` in about six seconds cold; the crate adds about 64.5 KB of wasm over a serde-only baseline, the dispatcher about 16 KB more. Shapes: plain handler 22 lines (15 code), typed handler with a hand-written forward-then adapter 48 (35), dispatcher 25 (19). The sans-I/O lib and its tests compiled first time from the `from_signed` snippet; the adapter took 2 (a `Sync` bound), the Workers crate 2 (an `on_error` closure before `build` fixes `E`, anticipated). Previous run: 5 resolved, 5 persist, 0 regressed.

- A hand-written adapter generic over another handler needs `MaybeSync`; rustc suggests `Sync`, which compiles natively and fails on `wasm32` with the crate's own "is not a handler over `Envelope`" message. The `Handler` docs name `MaybeSend` only.
- No combinator sequences two handlers; a typed handler handed to the receiver is 35 code lines of adapter, and the docs say so honestly.
- Driving the async `dispatch` from a sync entry point is still a noop-waker poll written by hand; the README says only "awaits it on whatever executor it has".
- Base64 inflation is undocumented as a cost: `raw` is +33.8% over the payload, the whole document +117.6% since the meta repeats payload fields.
- Wrong header-name casing presents as 401 on every delivery; the docs warn about case three times but never name the symptom.
- A plain envelope handler in a forward-everything receiver must guard on kind and action, or `decode_payload` fails every other kind; the README's one-event block guards on action only, right for its case and wrong for this one.
- `http::Request<String>` is an axum-free body for `receive` in tests; found by reading the receiver's source, not the docs.
- A native `cargo check` of a Workers crate fails on the `MaybeSend` bound, correctly; the worker example carries no rust-analyzer target hint.
- The crate ships no signing helper; the README's recipe suffices and `hmac`/`sha2` are already in the tree.
- README dependency snippet says `version = "0.2"` while the manifest says `0.1.0`.
- The payload-declaring macro expands to a three-line impl; judged marginal but fine, kept for its diagnostic.
