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

- `WebhookSecretError`: the error `str::parse::<WebhookSecret>` returns for
  an empty secret, for a deployment that reads its secret per request and
  answers instead of panicking.
- `Signature`: the `X-Hub-Signature-256` value parsed once, as the 32 MAC
  bytes. `str::parse` and `TryFrom<&[u8]>` refuse anything that is not
  `sha256=` and 64 hex digits as `SignatureError::Malformed`, `Display`
  renders the header value back, `Debug` is redacted, and equality is
  `subtle::ConstantTimeEq`.
- `Verifier::sign`: the `Signature` GitHub would send for a body, whose
  `to_string()` is the header value, so a test drives the receiver it built
  with no HMAC code of its own.
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
- The `header` module: the names of the headers the crate reads, as
  `http::HeaderName` constants, for a transport's pre-body signature check
  and a test's `http::Request::builder()`.
- `EventMeta::new` and `RepositoryRef::new` constructors.
- `From<&str>` on `EventKind`, `Action` and `TargetType`; `Display` on
  `TargetType` and `Match`; `Hash` on `EventMeta` and `RepositoryRef`.
- `Bytes` re-exported from the `bytes` crate.
- `ReceiveError::BodyRead` and `BodyError`: a body frame the transport could
  not produce is a receive error like every other pre-handler failure,
  answered 400 through `ReceiveError::status`, with the transport's error
  text as its `source()` and nothing else of the transport's type. Formerly
  the receiver returned the status directly.
- `ReceiveError::status`: the `http::StatusCode` the receiver answers each
  receive failure with, on the error itself, so a transport built on
  `Envelope::from_signed` answers GitHub as the receiver does. It replaces
  `ResponseStatus::for_receive_error`; see Removed.
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
  variant `MissingHeader { name }`, and `name` is an `http::HeaderName`, so
  an assertion compares it to a `header` constant by type; the `Display`
  text is unchanged.
- **Breaking:** `Envelope::from_signed` takes `&http::HeaderMap` where it
  took `&HeaderView`: `&HeaderView::from(&headers)` becomes `&headers`. The
  order of checks and the statuses are unchanged; a repeated header reads as
  its first value, a signature value that is not visible ASCII is
  `SignatureError::Malformed` (400), and any other header with such a value
  reads as absent.
- **Breaking:** The `http` feature is `http-body`, named for what it turns
  on: `WebhookReceiver`, its builder and `receive` over an `http_body::Body`.
  `features = ["http"]` becomes `["http-body"]`; `tower` implies it, and the
  default features are `http-body` and `derive`. The `http` crate itself is
  no longer optional: `from_signed` reads its `HeaderMap`, the `header`
  constants are its `HeaderName`s, and `ReceiveError::status` is its
  `StatusCode`. Every surveyed Rust runtime already depends on it
  non-optionally (`docs/research/header-abstractions.md`), and it adds one
  entry to the no-default dependency tree.
- **Breaking:** `Secret` is `WebhookSecret`: GitHub's term in full, beside
  `WebhookReceiver`, and no longer a collision with `secrecy::Secret` in a
  consumer's imports. `VerifyError` is `SignatureError`, named for its
  subject rather than the operation, since its `Missing` case is decided from
  the headers before anything is verified; its variants `MissingSignature`
  and `MalformedSignature` are `Missing` and `Malformed`, and
  `ReceiveError::Verify` is `ReceiveError::Signature`. `Verifier` keeps its
  name. The error messages are unchanged.
- **Breaking:** A `WebhookSecret` is never empty. `WebhookSecret::new` panics
  on empty bytes and `FromStr for WebhookSecret` returns
  `WebhookSecretError::Empty` where its error was `Infallible`;
  `Verifier::new` and `Verifier::also` have nothing left to refuse. An empty
  secret is the unset-environment-variable failure mode, and verifying
  against it would accept any sender who guessed the key.
- **Breaking:** `Verifier::verify` takes a `&Signature` where it took the
  header as `&str`, and fails only with `SignatureError::Mismatch`: a direct
  caller parses the header first, `header.parse::<Signature>()?`, and that
  parse is the one place `Malformed` comes from. `Missing` is decided from
  the headers by `Envelope::from_signed` and the receiver, `Malformed` by the
  parse, `Mismatch` by `verify`, each in exactly one place. The receiver now
  refuses a header that is not a signature before reading the body, as it
  already did an absent one; the statuses are unchanged, 400 and 401.
- **Breaking:** `tracing` feature: the `octoevents.verify` span's `outcome`
  is `verified` or `mismatch`; `malformed` is gone, since a malformed header
  is refused before the verifier is asked and opens no verify span. The
  receive span still records that refusal as `bad_request` with `error` =
  `malformed X-Hub-Signature-256 header`; before, whether a pre-HMAC refusal
  opened the verify span depended on which check caught it.
- **Breaking:** `tracing` feature: the `octoevents.receive` span records
  `outcome` as a label (`ok`, `unauthorized`, `bad_request`,
  `payload_too_large`, `handler_error`) and the HTTP status as a separate
  `status` field, where `outcome` was the status code; the
  `octoevents.dispatch` span names an unmatched delivery's outcome
  `unmatched_ok`/`unmatched_error` where it said
  `fallback_ok`/`fallback_error`.
- The default features are `http-body` and `derive`.
- The derive crate depends on `syn 3`, so a consumer with default features
  compiles one `syn`.

### Removed

- **Breaking:** `HeaderView`, shipped in 0.1.0 with `From<&http::HeaderMap>`
  and the setters; `Envelope::from_signed` takes the `http::HeaderMap`
  itself. Every Rust runtime surveyed hands over an `http::HeaderMap` or an
  `http::Request`, so the type stood between the headers a consumer held and
  the constructor that read them and did nothing `HeaderMap` does not; a
  consumer with `(name, value)` pairs collects them into a `HeaderMap`, and
  `HeaderName` parsing handles the case of the names.
- **Breaking:** `Envelope::from_signed_parts`; use `Envelope::from_signed`
  with the request's `HeaderMap`.
- **Breaking:** `ResponseStatus`, the five-variant status enum with
  `as_u16`, `for_receive_error` and `From<ResponseStatus> for
  http::StatusCode`; the receiver answers with `http::StatusCode` directly
  and `ReceiveError::status` maps a receive failure to one. The enum
  mirrored five `StatusCode` constants for a transport with a status type of
  its own, and the survey behind `HeaderView`'s removal found none: every
  runtime builds an `http::Response`. `ResponseStatus::for_receive_error(&e)`
  becomes `e.status()`, `NoContent` becomes `StatusCode::NO_CONTENT`, and
  `InternalServerError` becomes `StatusCode::INTERNAL_SERVER_ERROR`.
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
