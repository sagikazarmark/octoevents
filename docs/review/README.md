# Persona UX review

A repeatable review of the crate's user experience: four simulated users each build and compile a real program against the working tree, then an architect audits the design with their reports as evidence. Every finding is grounded in a compiler result or a quoted doc. The first run (2026-09-05) produced #18 and its tickets #19–#27.

Run it after a batch of API or documentation changes lands, before a release.

## Steps

1. **Confirm a green tree.** `cargo build --all-features` and `cargo test --all-features` pass on the tree under review. Record the commit hash for the report.

2. **Dispatch the four personas in parallel**, one subagent each. Each prompt names: the persona file under `personas/`, the shared rules in [`persona-rules.md`](persona-rules.md), the repository path, and a fresh scratch directory for that persona. Ask for the persona's report as its only message. Done when all four reports are in hand.

3. **Dispatch the architect** with [`architect.md`](architect.md) and the four persona reports pasted in as evidence. The architect runs after the personas because it weighs their usage data against the design. Done when its report is in hand.

4. **Synthesize.** Produce one report containing:
   - **Convergent findings**: anything three or more personas hit, with each persona's evidence.
   - **Baseline diff**: every baseline item in every persona file and the architect file, each marked *resolved*, *persists*, or *regressed*, with the evidence from this run. New findings that match no baseline item are listed as *new*.
   - **Per-reviewer top recommendations** as they reported them.
   - **Where the reviewers disagree**, and the synthesizer's own call on each.
   Done when every baseline item carries one of the three marks and every new finding is classified by severity.

5. **Publish.** Comment the synthesis on the tracking issue for the release under review, or open a new spec with the `to-spec` flow if the findings warrant one. Then update the **Baseline** section of each persona and architect file to this run's findings, so the next run diffs against this one.

## Files

- [`persona-rules.md`](persona-rules.md): rules and report format shared by every persona.
- `personas/`: one brief per persona — profile, goal, probes, baseline.
- [`architect.md`](architect.md): the architecture reviewer's brief and report format.
