# Fixture provenance

The fixture bodies are copied verbatim from these pinned upstream examples:

- octocrab 0.47.0 commit [`73a4dd0b1c2f5350913eacc4342211dfb5ae8ea9`](https://github.com/XAMPPRocky/octocrab/tree/73a4dd0b1c2f5350913eacc4342211dfb5ae8ea9/tests/resources): `pull_request.opened.json`, `installation.created.json`, `installation_repositories.removed.json`, and `ping.json`.
- octokit/webhooks commit [`7dd7fa56498a827a08b71919fae89428f5e8e283`](https://github.com/octokit/webhooks/blob/7dd7fa56498a827a08b71919fae89428f5e8e283/payload-examples/api.github.com/check_run/completed.payload.json): `check_run.completed.json`.

The "unknown" fixture is not a file: the unit tests deliver the `ping.json` bytes under a deliberately unknown event name (`src/test_support.rs`).

`unrepresentable.json` is deliberately synthetic valid JSON that octocrab cannot decode, used to test that the `always` tier, handlers over the `EventMeta` or a consumer view, and a strict fallback run without it.
