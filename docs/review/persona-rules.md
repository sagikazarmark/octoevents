# Persona rules

You are role-playing one user of the `octoevents` crate. Your brief is in `personas/`. Read it, then follow these rules.

## Grounding

Every claim in your report rests on something you compiled or something you quote. Write the program your persona would write, build it, and keep the compiler output. A doc that confused you is quoted; a term you had to look up is named; a source file you had to open is named with the reason.

Keep a **friction log** as you go: each entry is what happened, its severity (blocker / major / minor / nit), the evidence, and whether the docs had anticipated it. Count **compile iterations** to green for each approach you try.

Stay in character for reading order: your brief says what your persona reads first and when they give up and open source. Report from the persona's perspective; judge the crate on whether its abstractions fit your persona's task.

## Workspace

All writes go under the scratch directory named in your prompt. The repository is read.

Your project depends on the crate by path: `octoevents = { path = "<repository>", features = [..] }`. Set `CARGO_TARGET_DIR` to the repository's `target/` to reuse compiled dependencies; other personas may be building at the same time, and cargo waits on the lock rather than failing. For footprint or `wasm32` measurements use a separate target directory so the numbers are yours alone.

`gh webhook forward` is unavailable. A synthetic request is signed with `sha256=` plus the lowercase hex HMAC-SHA256 of the body under the secret, sent with the delivery, event and content-type headers. Before you use that recipe, record whether the crate's public docs would have led you to it.

## Baseline

Your brief ends with a **Baseline**: the friction items from the previous run. Probe each one on this tree. In your report, mark each *resolved* (with what you observed instead), *persists* (with fresh evidence), or *regressed* (worse than described). A finding that matches no baseline item is *new*.

## Report

Your report is your only message back. Sections, in order:

1. **Time-to-first-green**: compile iterations per approach, and the wall-clock feel.
2. **Friction log**: numbered, each with severity, evidence, and whether the docs anticipated it.
3. **What worked well**: specific.
4. **Baseline diff**: every baseline item marked resolved / persists / regressed, plus new items.
5. **Handler flavours verdict**: which flavours your persona needed, which it read about and never used, whether the split helped or hindered your task.
6. **Docs verdict**: what to cut, move up, or add for your persona.
7. **Top 5 recommendations**, ranked by impact on your persona, one sentence each.
8. **Your final code**, abridged to the shape a maintainer would want to see (about forty lines).

Your brief may add sections specific to its probes.
