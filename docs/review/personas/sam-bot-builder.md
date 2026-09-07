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

## Baseline (2026-09-07, `62cb533`)

Compile iterations: envelope handler 1; dispatcher 1 with `async fn` items over payload views and `Box<dyn Error + Send + Sync>`, 2 with a closure (bare `Ok(())`) and 2 with a struct handler carrying its own `io::Error`; tests 1; e2e 1; axum without `tower` 3 (the boxed error does not compile through `receive`; a newtype does). About 12 minutes of reading before the first line of code; the goal met in about 40. Previous run: 6 resolved, 6 persist, 0 regressed.

- `Dispatcher<Box<dyn Error + Send + Sync>>` handed to `receive` inside an axum closure is "implementation of `Send` is not general enough"; a newtype around the box compiles and so does the `tower` path. The README's hello-world error type and its own no-`tower` wiring do not compose, and nothing says so.
- The README's headline error observer prints "payload could not be decoded" without the serde field; the `source()` walk that shows it appears only for the thiserror error in the complete program.
- A struct handler with its own error type needs a `From` on the application error; the doc example teaching `type Error = std::io::Error` never registers it.
- Bare `Ok(())` in a closure: E0283, anticipated by the README; rustc suggests a literal `E`.
- A handler that already matched on kind by hand still needs `impl_payload!` for `decode_payload`; the kind-free `Envelope::decode` is named only in the cross-kind paragraph.
- Adding octocrab as a direct dependency and needing complete fixture payloads for its structs is still undocumented.
- Handing a payload handler straight to `build` is E0631 naming `Handler<Envelope>`, readable but with no hint to wrap it in a dispatcher.
- The README's testing section shows only `Match::Matched`; the unmatched variants are found in the dispatcher example.
- The "`Box<dyn Error>` is not itself an `Error`" paragraph arrives before the first route; vocabulary (tier, "route tier", observer, view) still arrives before the Handlers section defines it.
- The no-`tower` axum wiring is still filed under the "One event, one handler" heading; the Features table links to it.
- The 14-line hello world is not runnable alone (no `main`, no mount) and hands off to the complete program 60 lines down.
- README dependency snippet says `version = "0.2"` while the manifest says `0.1.0`.
