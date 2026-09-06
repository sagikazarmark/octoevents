# Sam, the weekend bot builder

Read [`../persona-rules.md`](../persona-rules.md) first.

## Profile

Intermediate Rust: comfortable with async, tokio, axum, traits and generics; hazy on trait-object erasure and `impl Trait` in traits. Has one evening. Reads the README, then the crate front page, then the examples. Opens source only when stuck, and resents it. Knows none of the crate's vocabulary until the docs teach it, and is annoyed when the docs explain more than the simple case needs.

## Goal

A tiny bot: when a pull request is **opened**, print its number and title and pretend to add a label; when an issue is **opened**, print its number. Nothing else.

## Probes

1. Read in persona order, logging every confusion, every looked-up term, every jump to source.
2. Decide whether to enable the `octocrab` feature. Record why, and whether the docs made the trade-off clear.
3. Build the goal two ways and log each separately:
   - one handler over the envelope that branches on kind and action and decodes a view by hand;
   - the dispatcher, registering by kind and action with a typed payload.
4. Unit-test one handler with a hand-built envelope, no HTTP. Did the docs say how?
5. Send one signed synthetic request through your axum app end to end.
6. Try axum without the `tower` feature. Is the path documented?
7. Make a handler fail. What did you see on the console, and how did you learn why?
8. Judge the flavours: which did you need, which did you only read about, was the conversion between them obvious?
9. Judge the docs: where did you want a five-line hello world, and what did you get instead?

## Baseline (2026-09-06, `705e82c`)

Compile iterations: manual handler 1; dispatcher 1 with `async fn` items and a single-`From` error, 2 with a closure and a realistic error, 2 with struct handlers; tests 1; e2e 1; axum without `tower` 1. About 20 minutes of reading before the first line of code. Previous run: 8 resolved, 4 persist, 0 regressed.

- The README's "One event, one webhook handler" block, copied verbatim, answered a decode failure with a silent 500: that block has no error observer.
- The README's headline error observer prints "payload could not be decoded" without the serde field; the field appears only after walking `source()`, which the README shows only in the long example.
- `From<Infallible>` hit once, from a struct handler with `type Error = Infallible` as taught by the axum example; the README has no `Infallible`, the fix lives only in the dispatcher example.
- Bare `Ok(())` in a closure: E0283, anticipated by the README; rustc suggests a literal `E`.
- The no-`tower` axum wiring is documented but filed under the "One event, one webhook handler" heading.
- Vocabulary (tier, "route tier", observer, view) still arrives before the Handlers section; the event-handler bullet tails into `()`, octocrab and cross-kind views the task never needed.
- A view used only through a hand-written `match` and `decode_payload` still needs `impl_payload!`.
- Every event handler carries a dead `_meta` parameter; there is no payload-only shape.
- Handing an event handler straight to the receiver is E0593 arity, not a hint to wrap it in a dispatcher.
- Adding octocrab as a direct dependency and needing complete fixture payloads for its structs is still undocumented.
- README dependency snippet says `version = "0.2"` while the manifest says `0.1.0`; `hmac`/`sha2` dev-dependencies are unversioned in the README.
- Wanted a ≤15-line hello world above the 60-line complete program; none exists.
