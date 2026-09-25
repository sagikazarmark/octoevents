# Observe refusals and handler failures

[Documentation index](../README.md#documentation)

Use this setup for a native server when GitHub reports a non-2xx status or
when you need to monitor accepted deliveries. HTTP responses intentionally
carry no diagnostic body.

## Configure a subscriber

Add `tracing` to octoevents' features and depend on the subscriber:

```toml
octoevents = { version = "0.3", features = ["tower", "tracing"] }
tracing-subscriber = "0.3"
```

Initialize once at startup, before serving requests:

```rust,no_run
use tracing_subscriber::fmt::format::FmtSpan;

tracing_subscriber::fmt()
    .with_max_level(tracing_subscriber::filter::LevelFilter::INFO)
    .with_span_events(FmtSpan::CLOSE)
    .init();
```

`INFO` enables receive and dispatch spans, including their late-recorded
`status`, `outcome` and refusal `error`. `FmtSpan::CLOSE` renders those fields
when the span closes. Without span output or a span exporter, an ordinary
event-only logger sees handler ERROR events but no refusals. Filtering at
ERROR also disables the receive span. Use DEBUG when you need the verify
span's body length, secret count and verification outcome.

## Check the setup without GitHub

From a repository clone:

```console
cargo run --example observability --features tracing
cargo test --example observability --features tracing
```

The [executable example](../examples/observability.rs) drives unsigned,
malformed-signature, oversized and signed failing requests through the same
subscriber configuration. Its test asserts both statuses and rendered
diagnostics. Expect these distinguishing fields (timestamps and formatting
are subscriber-dependent):

| Request | Status | Output to look for |
| --- | --- | --- |
| Missing signature | 401 | Receive span close: `outcome="unauthorized"`, `status=401`, refusal `error` |
| Malformed signature | 400 | Receive span close: `outcome="bad_request"`, `status=400` |
| Body over the configured limit | 413 | Receive span close: `outcome="payload_too_large"`, `status=413` |
| Signed request whose handler fails | 500 | `handler failed` ERROR event with the error; receive close with `outcome="handler_error"` |

A dispatcher adds the failing handler, registration site and tier to its
error; the subscriber renders the source chain beneath it. Decode failures
appear at the handler whose input needed the decode.

## Interpret results

- A 204 is acceptance, not proof that a route matched. With an asynchronous
  acceptance handler it means stored, not processed. Monitor processing
  separately as described in [Recovery](recovery.md).
- Default `ping` deliveries and successful or refused requests emit no
  application event. Span-close records above are subscriber-generated.
- Refusals happen before a handler runs, so reporting around `dispatch`
  cannot observe them. Custom dispatch reporting is demonstrated in the
  [integration guide](guide.md#error-handling).
- Header-derived delivery IDs and event names may appear before verification;
  treat them as untrusted diagnostic data.
- The crate does not record secrets or signatures. Application error text is
  recorded as supplied: avoid putting credentials or complete payloads in
  your errors, and configure retention/access accordingly.

The [API tracing reference](https://docs.rs/octoevents/0.3.0/octoevents/#tracing)
is authoritative for span names, fields and outcome vocabularies. For
Cloudflare console reporting, follow the [Worker guide](../examples/worker/README.md).
