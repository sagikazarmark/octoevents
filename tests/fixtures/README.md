# Fixture provenance

The fixture bodies are copied verbatim from these pinned upstream examples:

- octocrab 0.47.0 commit [`73a4dd0b1c2f5350913eacc4342211dfb5ae8ea9`](https://github.com/XAMPPRocky/octocrab/tree/73a4dd0b1c2f5350913eacc4342211dfb5ae8ea9/tests/resources): `pull_request.opened.json`, `installation.created.json`, `installation_repositories.removed.json`, and `ping.json`.
- octokit/webhooks commit [`7dd7fa56498a827a08b71919fae89428f5e8e283`](https://github.com/octokit/webhooks/blob/7dd7fa56498a827a08b71919fae89428f5e8e283/payload-examples/api.github.com/check_run/completed.payload.json): `check_run.completed.json`.
- `unknown.json` duplicates the pinned octocrab ping body and is delivered with a deliberately unknown event header.

`unrepresentable.json` is deliberately synthetic valid JSON used only to test the raw fallback.

`envelope.v0.1.0.json` is not a webhook body: it is an `Envelope` serialized
by octoevents v0.1.0 (`serde_json::to_string_pretty` over `Envelope::from_signed`
with the `BODY` of the `envelope` unit tests, delivery ID `delivery`, event
`pull_request`, target type `repository`, target ID `7`). It pins the shape
that nested four fields under `common`, which the legacy deserialize shim
accepts until 0.3.0.
