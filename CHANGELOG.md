# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The two crates, `octoevents` and `octoevents-derive`, share one version and
are released together; this file covers both.

<!-- next-header -->

## [Unreleased] - ReleaseDate

Almost every public type was reshaped since 0.1.0. A consumer moving forward
reads the "Changed" and "Removed" lists first.

### Added

- `SecretError`: the error `str::parse::<Secret>` returns for an empty
  secret, for a deployment that reads its secret per request and answers
  instead of panicking.
- `Verifier::sign`: the `X-Hub-Signature-256` value GitHub would send for a
  body, so a test drives the receiver it built with no HMAC code of its own.
- `Envelope::new`: an unverified envelope for a test, its meta read from the
  same bytes the receiver would read.
- `Envelope::decode_payload`: a kind-checked decode into any `Payload`.
- `Handler<I>`: one handler trait over any `FromEnvelope` input: the
  `Envelope`, the `EventMeta`, a `Payload` view or `Event<P>` for the meta
  beside the payload. Implemented by `async fn` items, structs, closures and
  `Arc<H>`.
- `Payload`, `FromEnvelope`, `Event<P>`, and `#[derive(Payload)]` from the
  new `octoevents-derive` crate behind the `derive` feature, on by default.
- `DispatcherBuilder::always`: a tier that runs for every delivery before
  routing.
- `DispatcherBuilder::on` takes any `IntoMatcher`: an `EventMatcher` shape
  (a kind, several kinds, a kind with actions, kind/action pairs) or, for a
  handler over a payload, an `Action`, an array of them or `AnyAction`.
- `Dispatcher::dispatch` reports an `Outcome`: the `Match` (`Matched`,
  `UnmatchedAction`, `UnmatchedKind`) beside the result.
- `DispatchError`: the failing handler's application error wrapped with the
  `Tier`, the delivery's ID, kind and action, the handler's name and its
  registration site.
- `DecodeError`: the one error of every decode path, with `KindMismatch`,
  `Json` and `Input` variants and the `input`/`input_with_source`
  constructors for a consumer's own `FromEnvelope` impl.
- `WebhookReceiverBuilder::on_error`: an observer called with the meta and
  the handler's error before the 500 is answered.
- `WebhookReceiverBuilder::trace_errors` and `trace_boxed_errors` (`tracing`
  feature): put the error's text and source on the failed-delivery event.
  `trace_errors` asks `TracedError`, a sealed trait every `Error` implements,
  and `trace_boxed_errors` asks `BoxedError`, an error behind a pointer or a
  `DispatchError` over one; asking `trace_errors` of a boxed error is a
  compile error that names `trace_boxed_errors`.
- `HeaderView::from_lookup`: a view built by asking a string map for each
  header this crate reads.
- The `header` module: the lowercase names of the headers the crate reads.
- `EventMeta::new` and `RepositoryRef::new` constructors.
- `From<&str>` on `EventKind`, `Action` and `TargetType`; `Display` on
  `TargetType` and `Match`; `Hash` on `EventMeta` and `RepositoryRef`.
- `Bytes` re-exported from the `bytes` crate.
- `ReceiveError::BodyRead` and `BodyError`: a body frame the transport could
  not produce is a receive error like every other pre-handler failure,
  answered 400 through `ResponseStatus::for_receive_error`, with the
  transport's error text as its `source()` and nothing else of the
  transport's type. Formerly the receiver returned the status directly.
- `tracing` feature: the `octoevents.dispatch` span records the tier,
  handler and registration site of a failure; a failed delivery emits one
  event at ERROR, `handler failed`.
- `tracing` feature: the `octoevents.receive` span records the text of the
  `ReceiveError` that refused a request as `error`, so a `bad_request` says
  which refusal it was. The error's source, for a body that could not be
  read the transport's own text, is not recorded.
- A `CHANGELOG.md`, and an `include` list so the published crate ships the
  sources, the `axum` and `dispatcher` examples, the tests with their
  fixtures, the README and the licences, and nothing else.

### Changed

- **Breaking:** `Envelope` is `#[non_exhaustive]` and composed of `meta:
  EventMeta` and `raw_payload: Bytes`. The former top-level fields
  (`delivery_id`, `kind`, `action`, `target_type`, `target_id`) and the
  nested `common` moved into `EventMeta`; `raw` is `raw_payload`, in the
  struct and on the wire. Envelopes come from `Envelope::from_signed`,
  `Envelope::new` or serde, never from a struct literal.
- **Breaking:** `Common` is `EventMeta`, which also carries the delivery ID,
  kind, action and target.
- **Breaking:** `Envelope::parse` is `Envelope::decode` and returns
  `DecodeError`; `Envelope::parse_typed` (`octocrab` feature) is
  `Envelope::decode_event`.
- **Breaking:** `WebhookHandler<E>` is `Handler<I>`, with the error as the
  associated type `Error` and the input as the type parameter. A handler
  over the envelope is `Handler<Envelope>`.
- **Breaking:** `DispatcherBuilder::on(kind, handler)` and
  `on_action(kind, action, handler)` are one `on(matcher, handler)`;
  `fallback` takes a handler over the envelope and may be called more than
  once, forming a chain.
- **Breaking:** `Dispatcher::dispatch` returns `Outcome<E>`, a
  `#[non_exhaustive]` struct, instead of `Result<(), E>`. The
  `Handler<Envelope>` impl still returns the plain result, and its error is
  `DispatchError<E>`.
- **Breaking:** `DispatcherBuilder::on` requires `E: From<DecodeError>`, the
  conversion of a failed decode for the handler's input; `always`,
  `fallback` and `build` do not, so a dispatcher of always and fallback
  handlers builds over any error type.
- **Breaking:** `WebhookReceiver<H, E>` is `WebhookReceiver<H>`; the error
  is the handler's.
- `WebhookReceiver::receive` and the Tower `Service` impl borrow the handler
  instead of cloning the receiver per delivery, so `H: Clone` is no longer
  required, and neither is `B: Unpin` of the body: the receiver pins the
  body where it polls it. `receive` is written as `fn -> impl Future +
  MaybeSend`, so its future is `Send` on native targets whenever the
  handler and body are.
- **Breaking:** `WebhookReceiver::receive` and the Tower `Service` impl
  require `B::Error: Display` of the body's error type, which `http_body`
  does not; hyper's, axum's and a Worker's error types and `Infallible`
  have it. What the receiver renders into `BodyError`.
- **Breaking:** `ReceiveError::MissingHeader(&'static str)` is the struct
  variant `MissingHeader { name }`.
- **Breaking:** A `Secret` is never empty. `Secret::new` panics on empty
  bytes and `FromStr for Secret` returns `SecretError::Empty` where its error
  was `Infallible`; `Verifier::new` and `Verifier::also` have nothing left to
  refuse. An empty secret is the unset-environment-variable failure mode, and
  verifying against it would accept any sender who guessed the key.
- **Breaking:** `tracing` feature: the `octoevents.receive` span records
  `outcome` as a label (`ok`, `unauthorized`, `bad_request`,
  `payload_too_large`, `handler_error`) and the HTTP status as a separate
  `status` field, where `outcome` was the status code; the
  `octoevents.dispatch` span names an unmatched delivery's outcome
  `unmatched_ok`/`unmatched_error` where it said
  `fallback_ok`/`fallback_error`.
- The default features are `http` and `derive`.
- The derive crate depends on `syn 3`, so a consumer with default features
  compiles one `syn`.

### Removed

- **Breaking:** `Envelope::from_signed_parts`; use `Envelope::from_signed`
  with `HeaderView::from(&headers)`.
- **Breaking:** `Default` on `RepositoryRef` and on the former `Common`; use
  the `new` constructors.

## [0.1.0] - 2026-09-02

The first release: `Verifier` and `Secret`, `Envelope::from_signed` over a
`HeaderView`, `WebhookReceiver` and its builder over `http` types with an
optional Tower `Service` impl, a `Dispatcher` routing by kind and action,
`EventKind` and `Action` as lossless string enums, and `octocrab` and
`tracing` as optional features.

<!-- next-url -->
[Unreleased]: https://github.com/sagikazarmark/octoevents/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sagikazarmark/octoevents/releases/tag/v0.1.0
