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

## Baseline (2026-09-05)

Compile iterations: manual handler 2; dispatcher 1 with a single-`From` error, 4 with a realistic error, 2 with struct handlers; e2e 1. About 25 minutes of reading before the first line of code.

- README's first code block is three handler impls with no receiver; "Quick start" is at the bottom and has no Rust.
- Crate front-page "Quick start" asserts a verification *failure* and reads as "how not to do it".
- The `tower` feature looked required for axum; `receive` inside a `post` closure worked first try and is undocumented.
- README lines ~97–117 are one 21-line paragraph on outcomes, decode rules and dead-lettering that the task never needed.
- Bare `Ok(())` in a closure: E0283 once the application error has two `From` impls; docs over-state it as "always".
- `From<Infallible>` boilerplate hit twice, once from a struct with `type Error = Infallible` copied from the README; absent from the README.
- A failing handler answered 500 with nothing on the console; the fix was a pasted twenty-line wrapper.
- The front-page headline example (a typed handler adapted straight into the receiver) answered 500 on the second event kind.
- No public recipe for signing a test request or for the four headers a synthetic request needs.
- Building an envelope by hand is documented on the `EventMeta` page, not where a tester looks.
- Adding octocrab as a direct dependency and needing complete fixture payloads for its structs was undocumented.
- Vocabulary wall (envelope, meta, tier, raw, always, fallback, outcome, match, registration site) arrives before the reader knows whether they need a dispatcher.
