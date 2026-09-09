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
  bytes. `TryFrom<&http::HeaderValue>`, `TryFrom<&[u8]>` and `str::parse`
  refuse anything that is not `sha256=` and 64 hex digits as
  `SignatureError::Malformed`, `Display` renders the header value back and
  `From<Signature> for http::HeaderValue` puts it on a request, marked
  sensitive, `Debug` is redacted, and it has no comparison: no `PartialEq`
  and no `ConstantTimeEq`, so `verifier.sign(body) == signature` and
  `.ct_eq(..)` do not compile and `Verifier::verify`, over every configured
  secret, is the verification path the crate offers. `subtle` is no longer
  part of the public API.
- `Verifier::sign`: the `Signature` GitHub would send for a body, which goes
  on a request's header as it is, so a test drives the receiver it built
  with no HMAC code of its own.
- `Envelope::new`: an unverified envelope for a test, its meta read from the
  same bytes the receiver would read.
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
- `DispatchError`: the failing handler's application error, boxed as a
  `BoxError`, wrapped with the `Tier`, the delivery's ID, kind and action,
  the handler's name and its registration site. An `Error` whatever the
  handler's error was, so a dispatcher registers as a route of another and a
  reporter walks one chain: `Display` says where, `source()` why. A policy
  that wants its own type back downcasts the source,
  `error.source.downcast_ref::<AppError>()`, and a decode failure is
  `downcast_ref::<DecodeError>()`.
- `BoxError`: the crate's erased error, `Box<dyn Error + Send + Sync>` on
  native targets and `Box<dyn Error>` on `wasm32`, where a Worker's error
  holds a `JsValue`. What every handler's error converts into where it is
  registered, and what `DispatchError::source` holds. A handler returning
  `Result<(), BoxError>` needs no error enum; one with an error type of its
  own keeps it, and the registration asks only `Into<BoxError>`, which every
  `Error + Send + Sync + 'static` is, and on `wasm32` every `Error +
  'static`.
- `DecodeError`: the one error of every decode path, with `KindMismatch`,
  `Json` and `Input` variants and the `input`/`input_with_source`
  constructors for a consumer's own `FromEnvelope` impl.
- `WebhookReceiverBuilder::on_error`: an observer called with the meta and
  the handler's error, as the handler returned it, before the 500 is
  answered. For what tracing does not do: a metric, a dead letter, a line on
  stderr.
- The `header` module: the names of the headers the crate reads, as
  `http::HeaderName` constants, for a transport's pre-body signature check
  and a test's `http::Request::builder()`. `CONTENT_TYPE` is
  `http::header::CONTENT_TYPE` re-exported, the others are GitHub's own.
- `EventMeta::new`, `RepositoryMeta::new` and `AccountMeta::new` constructors.
- `AccountMeta`: the account's meta `EventMeta::organization` and
  `EventMeta::sender` hold, the numeric `id` beside the `login`, with
  `Display` as the login.
- `From<&str>` and `From<String>` on `EventKind`, `Action` and `TargetType`;
  `Display` on `TargetType` and `Match`; `Hash` on `Envelope`, `EventMeta`
  and `RepositoryMeta`.
- `TryFrom<Vec<u8>>` and `TryFrom<&[u8]>` on `WebhookSecret`: the fallible
  constructors for a secret that is bytes rather than a string, one read
  from a file or a secret manager, refusing empty bytes as
  `WebhookSecretError::Empty` the way `str::parse` does. `WebhookSecret::new`
  stays the panicking form for a deployment that reads its secret at startup.
- `From<WebhookSecret>` and `Extend<WebhookSecret>` on `Verifier`: `new` as a
  conversion, for a builder taking `impl Into<Verifier>`, and `also` over an
  iterator, for a rotation window read from configuration as a list.
- `From<Vec<EventKind>>` and `From<Vec<(EventKind, Action)>>` on
  `EventMatcher`, for a route table whose size is known at run time.
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
- `WebhookReceiver::receive_bytes`: the receiver over the request's
  `http::HeaderMap` and its body as `Bytes` already read, answering with the
  `http::StatusCode`, for a transport with no `http_body::Body`, which is what
  every surveyed serverless runtime hands over. The whole contract `receive`
  applies (the header-only refusal, the body limit, `ping`, the handler, the
  `on_error` observer, the failed-delivery event and the receive span) over
  the two arguments `Envelope::from_signed` takes, in the core under every
  feature set. What `from_signed`'s docs showed as 45 lines of code to copy is
  this one call; `from_signed` stays for a transport that wants the envelope
  and not the answer.
- `tracing` feature: the `octoevents.dispatch` span records the tier,
  handler and registration site of a failure; a failed delivery emits one
  event at ERROR, `handler failed`, with the delivery's identifying fields
  (`delivery_id`, `event`, and `action` and `installation_id` when it has
  them) and the handler's error, boxed, as `error`, an error value whose
  chain of sources the subscriber renders (`error=<where>
  error.sources=[<why>, ..]` under the `fmt` subscriber). Unconditional: no
  setting and no observer is needed to see why a delivery failed. The 500 it
  is answered with is the receive span's `status`, not a field of the event:
  a handler failure is answered nothing else.
- `tracing` feature: the `octoevents.receive` span records the text of the
  `ReceiveError` that refused a request as `error`, so a `bad_request` says
  which refusal it was. The error's source, for a body that could not be
  read the transport's own text, is not recorded.
- A `CHANGELOG.md`, and an `include` list so the published crate ships the
  sources, the examples, the tests with their fixtures, the README and the
  licences, and nothing else.
- Examples in the order to read them, each with `#[cfg(test)]` tests that
  drive it without GitHub (`cargo test --examples`): `quickstart`, the
  README's program verbatim; `axum`, a receiver with no dispatcher;
  `dispatcher`, every tier and a handler over every input, the receiver's
  knobs set; and `policy_seam`, the former `dispatcher` example, whose store
  now holds the envelopes it claims to, so a redelivery after a handler
  failure is recoverable. The Cloudflare Worker example gained a README, a
  `[vars]` block and an editor hint for the wasm target.

### Changed

- **Breaking:** `Envelope` is `#[non_exhaustive]` and composed of `meta:
  EventMeta` and `raw_payload: Bytes`. The former top-level fields
  (`delivery_id`, `kind`, `action`, `target_type`, `target_id`) and the
  nested `common` moved into `EventMeta`; `raw` is `raw_payload`, in the
  struct and on the wire. Envelopes come from `Envelope::from_signed`,
  `Envelope::new` or serde, never from a struct literal.
- **Breaking:** `Common` is `EventMeta`, which also carries the delivery ID,
  kind, action and target.
- **Breaking:** `EventMeta::organization` and `EventMeta::sender` are
  `Option<AccountMeta>`, the account's numeric `id` beside its `login`, where
  they were the login alone. The ID is the identity a policy keys on (a
  tenant table, a bot allow-list); the login can be renamed under it, and a
  bare `String` could never grow a field. `repository` already kept its
  `id`. On the
  wire the two are objects with `id` and `login`, and an account object
  without an `id` reads as absent, as a `repository` without `full_name`
  does. `Display` on `AccountMeta` is the login, so a `{sender}` in a format
  string reads as before; `meta.sender.unwrap_or_default()` becomes
  `meta.sender.map(|s| s.login).unwrap_or_default()`.
- **Breaking:** `RepositoryRef` is `RepositoryMeta`, and the account type
  is `AccountMeta`: the meta of the repository or the account, the fields the
  probe keeps, beside `EventMeta`, which holds them. "Ref" is a Rust word
  (`&`, `std::cell::Ref`) and a GitHub word (`ref`, `ref_type`,
  `refs/heads/..` in `push`, `create` and `delete` payloads), and neither is
  what the type is. Neither is `#[non_exhaustive]`: they are the four and
  two fields they are, a test builds one as a literal, and a field GitHub
  adds is a breaking change here as it would be to the literal.
- **Breaking:** `Envelope::parse` is `Envelope::decode` and returns
  `DecodeError`, the kind-free decode a consumer's own `FromEnvelope` impl
  calls. `Envelope::parse_typed` (`octocrab` feature) is gone; octocrab's
  `WebhookEvent` decodes as every other input does,
  `WebhookEvent::from_envelope(&envelope)`. There is one way to decode an
  input, `FromEnvelope::from_envelope`, and no inherent method beside it: a
  `decode_payload` and a `decode_event` existed on the way here and each was
  called from nowhere but the `FromEnvelope` impl two lines above it.
- **Breaking:** `WebhookHandler<E>` is `Handler<I>`, with the error as the
  associated type `Error` and the input as the type parameter. A handler
  over the envelope is `Handler<Envelope>`.
- **Breaking:** `DispatcherBuilder::on(kind, handler)` and
  `on_action(kind, action, handler)` are one `on(matcher, handler)`;
  `fallback` takes a handler over the envelope and may be called more than
  once, forming a chain.
- **Breaking:** `Dispatcher<E>` is `Dispatcher`, with no error type
  parameter, and so are `DispatcherBuilder`, `DispatchError` and `Outcome`.
  Every handler's error is boxed as a `BoxError` where the handler is
  registered, so handlers with different error types share one dispatcher
  and no enum joins them; `on`, `always` and `fallback` ask
  `H::Error: Into<BoxError>` of each, which every `Error + Send + Sync +
  'static` is (every `Error + 'static` on `wasm32`, where the box is
  `Box<dyn Error>`), in place of `E: From<H::Error>` and `on`'s
  `E: From<DecodeError>`. A `Decode(#[from]
  DecodeError)` variant an application error carried for the dispatcher's
  sake is no longer needed. `Dispatcher::<AppError>::builder()` is
  `Dispatcher::builder()`.
- **Breaking:** `Dispatcher::dispatch` returns `Outcome`, a
  `#[non_exhaustive]` struct, instead of `Result<(), E>`. The
  `Handler<Envelope>` impl still returns the plain result, and its error is
  `DispatchError`. `Outcome` and `DispatchError` are `Debug` and not
  `Clone` or `PartialEq`, which the boxed source is not; a test reads
  `outcome.matched` and `outcome.result` rather than comparing whole
  outcomes.
- **Breaking:** `DispatchError::source` is a `BoxError` and `into_source`
  returns one. An `on_error` observer over a dispatcher that matched on its
  application error's variants downcasts the source first:
  `error.source.downcast_ref::<AppError>()`.
- **Breaking:** `WebhookReceiverBuilder::build` asks `H::Error:
  Into<BoxError>` of the receiver's handler, so the failed-delivery event can
  carry the error; a handler returning `()` or a struct without an `Error`
  impl is refused there. The `on_error` observer still receives the error as
  the handler returned it, before the conversion.
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
  on: `WebhookReceiver::receive` over an `http_body::Body`, answering an
  `http::Response`. `features = ["http"]` becomes `["http-body"]`; `tower`
  implies it, and the default features are `http-body` and `derive`. The
  receiver itself, `WebhookReceiverBuilder` and `receive_bytes` are in the
  core under every feature set, since only the body reading touches
  `http_body`. The
  `http` crate itself is no longer optional: `from_signed` reads its
  `HeaderMap`, the `header` constants are its `HeaderName`s, and
  `ReceiveError::status` is its `StatusCode`. Every surveyed Rust runtime
  already depends on it non-optionally
  (`docs/research/header-abstractions.md`), and it adds one entry to the
  no-default dependency tree.
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
- **Breaking:** `Default` on `RepositoryMeta` (then `RepositoryRef`) and on
  the former `Common`; use the `new` constructors or a literal.

## [0.1.0] - 2026-09-02

The first release: `Verifier` and `Secret`, `Envelope::from_signed` over a
`HeaderView`, `WebhookReceiver` and its builder over `http` types with an
optional Tower `Service` impl, a `Dispatcher` routing by kind and action,
`EventKind` and `Action` as lossless string enums, and `octocrab` and
`tracing` as optional features.

<!-- next-url -->
[Unreleased]: https://github.com/sagikazarmark/octoevents/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sagikazarmark/octoevents/releases/tag/v0.1.0
