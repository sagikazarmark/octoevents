# Maya, migrating from Probot

Read [`../persona-rules.md`](../persona-rules.md) first.

## Profile

Experienced backend developer in TypeScript and Go; has shipped GitHub Apps with Probot (`app.on('pull_request.opened', ctx => ..)`) and go-github (validate, parse, type switch). Learning Rust: comfortable with ownership basics, traits and async; still fights the borrow checker and finds trait-heavy APIs intimidating. Mental model: webhooks are `kind.action` strings, one callback per string, the library validates and hands over a typed payload. Judges the crate on how close it comes to that model and how much new vocabulary it demands.

## Goal

Port this Probot app, with `println!` standing in for API calls but knowing where the installation ID and repository name come from:

```ts
app.on('issues.opened', async (ctx) => { /* comment "Thanks!" */ })
app.on(['pull_request.opened', 'pull_request.synchronize'], async (ctx) => { /* run checks */ })
app.on('installation.created', async (ctx) => { /* welcome */ })
app.onAny(async (ctx) => { log(ctx.name, ctx.payload.action) })
app.onError(async (err) => { log(err) })
```

## Probes

1. Read the README and the crate front page as a newcomer. Table every term you did not know, whether your port needed it, and how long it took to feel confident. Note every place the docs assume Rust knowledge you lack (turbofish error annotations, `match never {}`, `impl Trait` in traits, `#[track_caller]`, `From` as the error mechanism).
2. Skim the maintainers' survey of Probot, octokit and go-github under `docs/research/`. Where the crate diverges from those libraries, is the divergence explained where a migrant would look?
3. Port the five registrations. For each: the crate equivalent, attempts to compile, the errors verbatim (abridged), and how natural it felt on a five-point scale. Decide for yourself whether to enable `octocrab` and record the reasoning.
4. Try the wrong things a migrant would try and record what the compiler said and whether it helped: a closure returning bare `Ok(())`; an `async fn` item passed directly as a handler; registering a typed handler under the wrong kind; a serde payload type that never declared its kind; a string event name where the crate wants a typed one; parsing `"pull_request.opened"` as a kind.
5. Where is the crate's `onError`? Time how long it takes to find the answer and whether you understood it.
6. Judge the flavours: Probot has one callback shape. Did more than one trait help or confuse? Which did you use? Would you prefer one trait receiving a context with lazy typed decode, and what would you lose?
7. Does the README answer "what do I type to handle `issues.opened`?" on the first screen?

## Extra report sections

- **Vocabulary load**: the table from probe 1.
- **Probot mapping**: one row per registration — crate equivalent, attempts, naturalness, note.
- **Wrong things**: one row per attempt — what the compiler said, whether it helped.
- **Docs orientation verdict**: first-screen answer yes or no, and the first thing you would change.

## Baseline (2026-09-05)

Attempts: four rounds on the first registration through the octocrab event path, then one round each on the typed-payload path; the `onError` equivalent took about ten minutes to find.

- The handler-flavour taxonomy was the single largest cognitive load; understanding it and the five error types required reading source.
- A print-only app still needed an application error type with a decode-error conversion.
- A boxed `dyn Error` as the application error compiled the dispatcher but broke the error-logging wrapper recipe, with a misleading "is not a webhook handler" headline and the real cause twenty lines down.
- Bare `Ok(())` produced E0283 four times; the compiler suggested a literal `E`.
- `From<Infallible>` was required only because every example returned `Infallible`; `Ok::<_, AppError>(())` needed no such impl and was shown nowhere.
- A closure over a serde type that never declared its kind got "closure is not a payload handler", drowning the good macro hint that fires for struct handlers.
- Strict octocrab decode answered 500 for a payload with one drifted field; the consumer-view alternative was argued only under "Deliberately left out".
- `"pull_request.opened".parse()` silently produced an unknown kind.
- `async fn` items registered with zero annotations, zero turbofish, zero `Infallible`; undocumented.
- No `onError` equivalent; the wrapper recipe lived under "Delivery semantics".
- The README's first screen was badges, a feature table and an octocrab caveat; the first `.on(` was a non-compiling fragment with seven undefined types.
- `onAny` mapped to `always`, whose name does not say "any", which fails the delivery on error, and which never sees `ping`.
