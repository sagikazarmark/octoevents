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

## Baseline (2026-09-06, `705e82c`)

All five registrations compiled in one round with serde views and no octocrab, about 2.5 minutes from opening the README; the octocrab path took three rounds, both failures inside octocrab's model. The `onError` equivalent was found in under a minute. Previous run: 8 resolved, 4 persist, 0 regressed.

- `issues.opened` is spelled across three places (the kind on the payload type, a doc comment, the action at the registration); the registration line never says `issues`.
- Ceremony before the first handler for a print-only app: a thiserror derive, `#[error(transparent)]`, `#[from]`, a serde derive and a macro; the README never says a boxed `dyn Error` works as the application error, though it does.
- octocrab's `installation` payload struct has no `installation` field, and its `EventInstallation` is an enum; the README's Features row gives no hint. Strict octocrab decode answered 500 for a real fixture with one drifted field.
- Fail-fast on the first handler error is stated but never contrasted with Probot's run-all.
- `always` never sees `ping` by default; `handle_ping(true)` is not in the README; nothing maps `onAny` to `always`.
- Two rustdoc examples (`FromEnvelope` on the payload page, the event-handler page) compile only through hidden `From<Infallible>` lines; copying the visible lines fails E0277. The dispatcher prose still teaches `match never {}`.
- Registering a payload view under the wrong kind through `on` is a runtime kind mismatch ("expected a issues event"), not a compile error.
- `"pull_request.opened".parse::<EventKind>()` is silently an unknown kind.
- Bare `Ok(())` in a closure: E0283 with an unusable `E` suggestion; anticipated by the README.
- `RepositoryRef` is all-or-nothing on a partial `repository` object.
- README dependency snippet says `version = "0.2"` while the manifest says `0.1.0`; the survey table still says the crate has no `onError` observer.
