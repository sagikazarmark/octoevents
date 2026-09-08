# Header abstractions in webhook receivers

**Question.** How do other libraries decouple webhook signature verification
and header-derived metadata from any one HTTP framework's header container,
and is there a better or simpler design than `HeaderView<'a>`: in footprint,
ergonomics, the header-name-case footgun, or the malformed-versus-missing
signature distinction?

**Date.** 2026-09-08.

**Method.** Primary sources only: source files on GitHub, `Cargo.toml`s,
crates.io metadata, docs.rs, official platform docs, as fetched on the date
above. Where README and code differ, the code is reported. Two local
measurements were taken in this repo (`cargo tree` crate counts and a
`wasm32-unknown-unknown` micro-build); they are labelled as such. Each
section ends with its sources. Where a fetch failed or a claim could not be
verified, the section says so.

**Companion.** [webhook-libraries.md](./webhook-libraries.md) surveys the
same libraries for verification, routing and error handling;
[webhook-terminology.md](./webhook-terminology.md) for vocabulary. This
document covers only the header seam.

---

## 1. What is being evaluated

`HeaderView<'a>` (`src/envelope.rs:135-341`) holds six `Option<Cow<'a, str>>`
values and a `malformed_signature: bool`. It is built three ways: `From<&http::HeaderMap>`
(behind the default-on `http` feature), `from_lookup(|name| ...)` (the crate
calls the closure once per lowercase constant in `src/header.rs`), and chained
setters. Header-name case is the caller's concern; the docs say so and name
the symptom (401 on every delivery). `Debug` prints `[REDACTED]` for the
signature. The receiver (`src/service.rs:512`) builds it from `parts.headers`,
records `delivery_id` and `event_name` on the span, and refuses an unsigned
request before reading the body.

The `http` feature costs, in this repo today (`cargo tree -e normal`, unique
crates): 32 with no default features, 37 with `--features http`. The five
added are `http`, `http-body`, `http-body-util`, `futures-core`,
`pin-project-lite`. `bytes` and `itoa`, the only runtime dependencies of
`http` 1.5.0, are already in the no-default tree (`bytes` is a direct
dependency; `itoa` comes with `serde_json`). So the `http` *crate* alone is +1
crate; the other four are the receiver's body handling.

**Sources.** `src/envelope.rs`, `src/header.rs`, `src/service.rs`,
`Cargo.toml`, `cargo tree` in this repo at `258bfef`.

---

## 2. Rust webhook-verification crates: what do they take for headers?

| Crate | Verifying function | Header type | `http` dep | Missing vs malformed |
|---|---|---|---|---|
| `svix` 2.3.0 | `Webhook::verify(&self, payload: &[u8], headers: &HeaderMap)` | `http::HeaderMap` | required (`http = "1.1.14"`) | `MissingHeader(&'static str)` vs `InvalidHeader(&'static str)` when `to_str()` fails |
| `standardwebhooks` 1.0.2 | `Webhook::verify(&self, payload: &[u8], headers: &HeaderMap)` | `http::HeaderMap` | required (`http = "1.0"`) | same two variants |
| `async-stripe` (`async-stripe-webhook`) | `Webhook::construct_event(payload: &str, sig: &str, secret: &str)` | header **value** as `&str` | none | caller's problem; `Signature::parse` returns `BadSignature` for a bad format |
| `octocrab` | `WebhookEvent::try_from_header_and_body<B>(header: &str, body: &B)` | header **value** as `&str` | n/a (no verification) | n/a |
| `slack-morphism` | `SlackEventSignatureVerifier::verify(&self, hash: &str, body: &str, ts: &str)` | header **values** as `&str` | none | caller's problem; an `AbsentSignatureError` variant exists but is not produced by `verify` |
| `axum-github-webhook-extract` 0.3.0 | axum `FromRequest` | `http::HeaderMap` via axum | required | `get("X-Hub-Signature-256").and_then(to_str().ok())` collapses both into `"signature missing"` |
| `tower-github-webhook` 0.2.0 | Tower `Service<Request<B>>` | `http::request::Parts` | required | `get("x-hub-signature-256")` then `as_bytes().splitn(2, b'=')`: missing is one message, unparseable another; non-ASCII bytes are never checked because it works on bytes |
| `github_webhook_message_validator` 0.1.6 | `validate(secret: &[u8], signature: &[u8], message: &[u8]) -> bool` | decoded signature **bytes** | none | none; HMAC-SHA1 only |
| `twilio` 1.1.0 | `Client::parse_request<T>(&self, req: hyper::Request<Body>)` | whole `hyper::Request` | via hyper | `get("X-Twilio-Signature")` missing is `AuthError`; base64 failure is `BadRequest` |

Not found: `gh-webhook`, `rocket-github-webhook`, `actix-github-webhook`,
`github-webhook-verify` (crates.io returns no crate). `hubcaps` 0.6.2 has a
`hooks.rs` for managing webhooks through the API and no receiving or
verification code. `github-webhook` 0.5.2 (sksat) is generated payload types
only.

The two crates that took a container took `http::HeaderMap` and made `http`
non-optional. Both distinguish missing from non-visible-ASCII, with the header's
role in the error rather than its name:

```rust
// svix rust/src/webhooks.rs
fn get_header<'a>(headers: &'a HeaderMap, svix_hdr: &'static str, unbranded_hdr: &'static str, err_name: &'static str)
    -> Result<&'a str, WebhookError> {
    headers.get(svix_hdr).or_else(|| headers.get(unbranded_hdr))
        .ok_or(WebhookError::MissingHeader(err_name))?
        .to_str().map_err(|_| WebhookError::InvalidHeader(err_name))
}
```

Both also redact the key in `Debug`:

```rust
// standard-webhooks libraries/rust/src/lib.rs
impl Debug for Webhook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("Webhook { key: [REDACTED] }") }
}
```

Every crate that took values took them positionally as `&str`. `async-stripe`'s
`(payload: &str, sig: &str, secret: &str)` is three adjacent `&str`s; slack's
`(hash, body, ts)` likewise. No incident of a transposition was found in the
sources read, but none of these libraries documents the shape as a risk either;
the concern in `HeaderView::new`'s docs is a priori.

**Sources.**
[svix `rust/src/webhooks.rs`](https://github.com/svix/svix-webhooks/blob/main/rust/src/webhooks.rs),
[svix `rust/Cargo.toml`](https://github.com/svix/svix-webhooks/blob/main/rust/Cargo.toml),
[standardwebhooks `libraries/rust/src/lib.rs`](https://github.com/standard-webhooks/standard-webhooks/blob/main/libraries/rust/src/lib.rs),
[standardwebhooks `libraries/rust/Cargo.toml`](https://github.com/standard-webhooks/standard-webhooks/blob/main/libraries/rust/Cargo.toml),
[async-stripe `async-stripe-webhook/src/webhook.rs`](https://github.com/arlyon/async-stripe/blob/master/async-stripe-webhook/src/webhook.rs),
[octocrab `src/models/webhook_events.rs`](https://github.com/XAMPPRocky/octocrab/blob/main/src/models/webhook_events.rs),
[slack-morphism `src/signature_verifier.rs`](https://github.com/abdolence/slack-morphism-rust/blob/master/src/signature_verifier.rs),
[axum-github-webhook-extract `src/lib.rs`](https://github.com/daaku/axum-github-webhook-extract/blob/main/src/lib.rs),
[tower-github-webhook `src/future.rs`](https://github.com/SebRollen/tower-github-webhook/blob/main/src/future.rs),
[github_webhook_message_validator `src/lib.rs`](https://github.com/qubyte/github_webhook_message_validator/blob/master/src/lib.rs),
[twilio-rs `src/webhook.rs`](https://github.com/neil-lobracco/twilio-rs/blob/master/src/webhook.rs),
crates.io API for each name.

---

## 3. Rust serverless and edge runtimes: do they already speak `http`?

| Runtime | Request type | `http` crate | How a header is read otherwise |
|---|---|---|---|
| `worker` 0.8.5 (Cloudflare) | `worker::Request` wrapping `web_sys::Request`; with the `http` feature also `worker::HttpRequest = http::Request<worker::Body>` | **required** (`http.workspace = true`, not optional; the `http` feature only switches the macro's request type) | `Headers::get(&self, name: &str) -> Result<Option<String>>` (allocates; JS `Headers.get` is case-insensitive). `From<&Headers> for http::HeaderMap` and the reverse exist |
| `lambda_http` 1.3.1 | `pub type Request = http::Request<Body>` | required | n/a |
| `aws_lambda_events` 1.2 (raw Lambda) | `ApiGatewayProxyRequest { headers: HeaderMap, multi_value_headers: HeaderMap, .. }`, `ApiGatewayV2httpRequest { headers: HeaderMap, .. }` | required by the `apigw`/`alb` features | n/a; a string map arises only if the consumer parses the event JSON with their own serde struct |
| `spin-sdk` 4.0.0 | own `Request { headers: HashMap<String, HeaderValue>, .. }`; handlers may also take `hyperium::Request<B>` | required (`hyperium = { package = "http" }`) | `header(&self, name: &str) -> Option<&HeaderValue>` does `self.headers.get(&name.to_lowercase())`; `HeaderValue::as_str() -> Option<&str>` (a bytes variant exists) |
| `spin-sdk` 7.0.0 (current) | `pub use wasip3::http_compat::Request`, which is `pub type Request<T = IncomingRequestBody> = http::Request<T>` | required | n/a |
| `fastly` 0.13.1 | `fastly::Request` (own type); `From<http::Request<Body>>` and `From<Request> for http::Request<Body>` | required (`http = "^1.1.0"`) | `get_header(&self, name: impl ToHeaderName) -> Option<&HeaderValue>`, `get_header_str(..) -> Option<&str>`, `get_header_str_lossy(..) -> Option<Cow<str>>`; `HeaderValue` is `http`'s |
| `vercel_runtime` | re-exports `lambda_http::Request` | required (through `lambda_http`) | n/a |
| `wstd` 0.6.8 (wasi-http p2) | `pub use http::request::{Builder, Request}`; `pub use http::header::{HeaderMap, HeaderName, HeaderValue}` | required | n/a |

**Conclusion.** No surveyed Rust deployment target lacks `http::HeaderMap`.
Every one depends on `http` non-optionally and either *is* `http::Request` or
converts to it with a `From`/`TryFrom` impl. The repo's own Workers example
already runs the `http` path (`examples/worker/Cargo.toml` enables `http` on
both `worker` and `octoevents`). The only way to hold headers as a plain string
map is to bypass the platform SDK and deserialize the raw invocation JSON
oneself; for API Gateway's HTTP API that JSON has lowercase keys by contract
("All header names are lowercased", payload format 2.0 docs). The persona's
premise ("Lambda hands him headers as a string map") describes that hand-rolled
path, not `lambda_http` or `aws_lambda_events`.

**Sources.**
[workers-rs `worker/src/headers.rs`](https://github.com/cloudflare/workers-rs/blob/main/worker/src/headers.rs),
[workers-rs `worker/Cargo.toml`](https://github.com/cloudflare/workers-rs/blob/main/worker/Cargo.toml),
[workers-rs `worker/src/lib.rs`](https://github.com/cloudflare/workers-rs/blob/main/worker/src/lib.rs) (`HttpRequest` alias),
[workers-rs `worker/src/request.rs`](https://github.com/cloudflare/workers-rs/blob/main/worker/src/request.rs),
[aws-lambda-rust-runtime `lambda-http/src/lib.rs`](https://github.com/awslabs/aws-lambda-rust-runtime/blob/main/lambda-http/src/lib.rs),
[aws-lambda-rust-runtime `lambda-http/Cargo.toml`](https://github.com/awslabs/aws-lambda-rust-runtime/blob/main/lambda-http/Cargo.toml),
[aws-lambda-rust-runtime `lambda-events/src/event/apigw/mod.rs`](https://github.com/awslabs/aws-lambda-rust-runtime/blob/main/lambda-events/src/event/apigw/mod.rs),
[spin-rust-sdk v4.0.0 `src/http.rs`](https://github.com/spinframework/spin-rust-sdk/blob/v4.0.0/src/http.rs),
[spin-rust-sdk v4.0.0 `Cargo.toml`](https://github.com/spinframework/spin-rust-sdk/blob/v4.0.0/Cargo.toml),
[spin-rust-sdk main `crates/spin-sdk/src/http.rs`](https://github.com/spinframework/spin-rust-sdk/blob/main/crates/spin-sdk/src/http.rs),
[wasi-rs `crates/wasip3/src/http_compat/mod.rs`](https://github.com/bytecodealliance/wasi-rs/blob/main/crates/wasip3/src/http_compat/mod.rs),
[docs.rs `fastly` 0.13.1 `Request`](https://docs.rs/fastly/0.13.1/fastly/struct.Request.html) and its `Cargo.toml` on docs.rs,
[vercel-community/rust `crates/vercel_runtime/src/lib.rs`](https://github.com/vercel-community/rust/blob/main/crates/vercel_runtime/src/lib.rs),
[wstd `src/http/request.rs`](https://github.com/bytecodealliance/wstd/blob/main/src/http/request.rs),
[wstd `src/http/fields.rs`](https://github.com/bytecodealliance/wstd/blob/main/src/http/fields.rs),
[wstd `Cargo.toml`](https://github.com/bytecodealliance/wstd/blob/main/Cargo.toml),
[AWS API Gateway HTTP API Lambda integration payload formats](https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-develop-integrations-lambda.html).

---

## 4. The `http` crate's footprint

From `hyperium/http` master (`Cargo.toml` at version 1.5.0, released
2026-07-29):

- Dependencies: `bytes = "1"`, `itoa = "1"`. `fnv` was a dependency through
  1.3.1 and is gone from 1.4.0 onward (crates.io dependency lists for 1.3.1
  and 1.4.0). The CHANGELOG does not mention the removal.
- Features: `default = ["std"]`, `std = []`. `no_std` is not supported:
  `src/lib.rs` has the `cfg_attr(no_std)` commented out and
  `compile_error!("`std` feature currently required, support for `no_std` may be added later")`.
- MSRV: `rust-version = "1.57.0"` (raised from 1.49 at 1.4.0).

`HeaderMap::get` accepts `&str` and matches case-insensitively without
allocating. `impl Sealed for &str` calls
`HdrName::from_bytes(self.as_bytes(), |hdr| map.find(&hdr))`; `parse_hdr`
copies the name through the `HEADER_CHARS` lowercasing table into a 64-byte
stack scratch buffer (`SCRATCH_BUF_SIZE`), yielding a borrowed `HdrName` with
`lower: true`. Names longer than 64 bytes are kept as-is with `lower: false`
and compared with a table-driven `eq_ignore_ascii_case`; an invalid character
makes `find` return `None` (`unwrap_or(None)`), never an error. All six names
this crate reads are under 64 bytes.

`HeaderName::from_bytes` (the owned form) does allocate for custom names:
`Bytes::copy_from_slice(buf)` for the lowercase case, a `BytesMut` copy
otherwise. `x-github-*` and `x-hub-signature-256` are custom (not in the
`StandardHeader` table), so building a `HeaderName` for them allocates;
looking them up by `&str` does not. `HeaderView`'s `From<&HeaderMap>` uses the
`&str` path.

Local measurement (this machine, `rustc 1.98.0`, `wasm32-unknown-unknown`,
`opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`,
cdylib): a function that builds a `HashMap<String, String>` from text and looks
up the six names is 33,451 bytes of wasm; the same function building an
`http::HeaderMap` (`HeaderName::from_bytes` + `HeaderValue::from_str` +
`append`) and looking up the six names with `get(&str).to_str()` is 55,935
bytes. The delta of about 22 KB is an upper bound for `http` on a target that
does not already link it; on every runtime in section 3 the host SDK links
`HeaderMap` already, so the marginal cost to the consumer's binary is the
lookup code alone. The persona's baseline measured the whole crate at about
64.5 KB over a serde-only baseline.

**Sources.**
[hyperium/http `Cargo.toml`](https://github.com/hyperium/http/blob/master/Cargo.toml),
[hyperium/http `src/lib.rs`](https://github.com/hyperium/http/blob/master/src/lib.rs) lines 156-160,
[hyperium/http `CHANGELOG.md`](https://github.com/hyperium/http/blob/master/CHANGELOG.md),
[hyperium/http `src/header/map.rs`](https://github.com/hyperium/http/blob/master/src/header/map.rs) (`get`, `find`, `impl Sealed for &str`),
[hyperium/http `src/header/name.rs`](https://github.com/hyperium/http/blob/master/src/header/name.rs) (`parse_hdr`, `HdrName::from_bytes`, `HeaderName::from_bytes`, `SCRATCH_BUF_SIZE`),
crates.io dependency lists for `http` 1.3.1, 1.4.0, 1.5.0; local wasm build in `/tmp/opencode/wasmsize`.

---

## 5. Sans-I/O GitHub-webhook libraries in other languages

### 5.1 gidgethub (Python)

```python
# gidgethub/sansio.py
@classmethod
def from_http(cls, headers: Mapping[str, str], body: bytes, *, secret: Optional[str] = None) -> "Event":
    """...The mapping providing the headers is expected to support lowercase keys...."""
    signature = headers.get("x-hub-signature-256", headers.get("x-hub-signature"))
    if signature is not None:
        if secret is None:
            raise ValidationFailure("secret not provided")
        validate_event(body, signature=signature, secret=secret)
    elif secret is not None:
        raise ValidationFailure("signature is missing")
    try:
        data = _decode_body(headers["content-type"], body, strict=True)
    except (KeyError, ValueError) as exc:
        raise BadRequest(http.HTTPStatus(415), ..., headers=headers) from exc
    return cls(data, event=headers["x-github-event"], delivery_id=headers["x-github-delivery"])
```

- Keys read: `x-hub-signature-256` (falling back to `x-hub-signature`),
  `content-type`, `x-github-event`, `x-github-delivery`; all lowercase.
- Case: "The mapping providing the headers is expected to support lowercase
  keys" (docstring and `docs/sansio.rst`). The same sentence appears on
  `RateLimit.from_http` and `decipher_response`. It is the caller's job, as in
  `HeaderView::from_lookup`; the documented example passes aiohttp's
  `request.headers` (a `CIMultiDictProxy`, case-insensitive), so the contract
  is met by the framework, not by the consumer's own code.
- Missing signature with a secret configured: `ValidationFailure("signature is missing")`.
  Signature present with no secret: `ValidationFailure("secret not provided")`.
  Missing `content-type`: `KeyError` caught and turned into `BadRequest(415)`.
  Missing `x-github-event` or `x-github-delivery`: an uncaught `KeyError`.
- No malformed-versus-missing distinction is possible: a Python `str` mapping
  cannot hold non-text bytes.

### 5.2 Octokit.Webhooks (.NET)

```csharp
// src/Octokit.Webhooks/WebhookHeaders.cs
public sealed class WebhookHeaders {
    public string? UserAgent { get; init; }
    public string? Delivery { get; init; }
    public string? Event { get; init; }
    public string? HookId { get; init; }
    public string? HookInstallationTargetId { get; init; }
    public string? HookInstallationTargetType { get; init; }
    public string? Signature256 { get; init; }

    public static WebhookHeaders Parse(IDictionary<string, StringValues> headers) {
        ArgumentNullException.ThrowIfNull(headers);
        headers.TryGetValue("User-Agent", out var userAgent);
        headers.TryGetValue("X-GitHub-Delivery", out var delivery);
        headers.TryGetValue("X-GitHub-Event", out var eventName);
        headers.TryGetValue("X-GitHub-Hook-ID", out var hookId);
        headers.TryGetValue("X-GitHub-Hook-Installation-Target-ID", out var hookInstallationTargetId);
        headers.TryGetValue("X-GitHub-Hook-Installation-Target-Type", out var hookInstallationTargetType);
        headers.TryGetValue("X-Hub-Signature-256", out var signature256);
        return new WebhookHeaders { UserAgent = userAgent.ToString(), Delivery = delivery.ToString(), /* ... */ Signature256 = signature256.ToString() };
    }
}
```

- The closest structural analogue of `HeaderView`: a named-fields struct of
  values, seven of them (`HookId` and `UserAgent` beyond octoevents' set; no
  `Content-Type`), each `string?`.
- Case: `Parse` looks up the **canonical mixed-case** names and compares
  nothing itself. It does not lowercase and does not use
  `StringComparer.OrdinalIgnoreCase`; it relies on the dictionary's comparer.
  The two shipped adapters make that hold: ASP.NET Core passes
  `context.Request.Headers` (`IHeaderDictionary`, case-insensitive by
  contract), and Azure Functions rebuilds the map explicitly:

  ```csharp
  // src/Octokit.Webhooks.AzureFunctions/GitHubWebhooksHttpFunction.cs
  var headers = req.Headers.ToDictionary(kv => kv.Key, kv => new StringValues([.. kv.Value]), StringComparer.OrdinalIgnoreCase);
  await service.ProcessWebhookAsync(headers, (ReadOnlyMemory<byte>)body, ctx.CancellationToken)
  ```

  Same stance as octoevents and gidgethub (the container is responsible), but
  the library ships the adapters that discharge it.
- Missing headers: `StringValues.ToString()` returns `string.Empty` for a
  default value (`GetStringValue() ?? string.Empty` in dotnet/runtime), so every
  property is `""` rather than `null` when absent. Missing and empty are the
  same. `ProcessWebhookAsync` throws `ArgumentException("X-GitHub-Event header is missing or empty.")`
  when `Event` is null or whitespace.
- Signature: not on `WebhookHeaders`' path at all. The adapters call
  `WebhookSignatureValidator.Verify(string? signatureHeader, string? secret, ReadOnlySpan<byte> bodyUtf8)`,
  which takes the header **value**. Outcomes: `Valid`, `MissingSignature`
  (no signature, secret configured), `MissingSecret` (signature, no secret),
  `SignatureMismatch` (wrong prefix, wrong length, bad hex, or HMAC mismatch,
  all collapsed).
- `ProcessWebhookAsync` overloads: `(IDictionary<string, StringValues> headers, string body, CancellationToken)`,
  `(IDictionary<string, StringValues> headers, ReadOnlyMemory<byte> body, CancellationToken)`,
  and `(WebhookHeaders headers, WebhookEvent webhookEvent, CancellationToken)`.
  The second is the byte fast path; the third is the post-parse dispatch by
  event class.

### 5.3 `@octokit/webhooks` and `@octokit/webhooks-methods` (JavaScript)

- Core: `verifyAndReceive(state, event: { id, name, payload: string, signature })`,
  a named-fields object of **values**. Verification is
  `verifyWithFallback(state.secret, event.payload, event.signature, state.additionalSecrets)`.
- `@octokit/webhooks-methods` `verify(secret: string, eventPayload: string, signature: string)`:
  three positional strings; a falsy `signature` is a `TypeError`, and the
  `sha256=` prefix is stripped with `replace`, so a malformed value fails
  inside `hexToUInt8Array`.
- Middleware (`src/middleware/create-middleware.ts`) requires
  `WEBHOOK_HEADERS = ["x-github-event", "x-hub-signature-256", "x-github-delivery"]`
  and `content-type`; a missing one is a 400 `Required headers missing: ...`.
  The header read is injected per platform:

  ```ts
  // src/middleware/node/get-request-header.ts
  export function getRequestHeader<T = string>(request: any, key: string) { return request.headers[key] as T; }
  // src/middleware/web/get-request-header.ts
  export function getRequestHeader<T = string>(request: Request, key: string) { return request.headers.get(key) as T; }
  ```

  Node's `IncomingMessage.headers` lowercases; Fetch `Headers.get` is
  case-insensitive. The library never compares a name itself; it only ever
  asks a case-insensitive container for a lowercase key. Same structure as
  `from_lookup`, with the platform adapter supplied by the library.
- The two files the brief named (`get-missing-headers.ts`, `middleware.ts`)
  no longer exist on `main` (404); the logic lives in `create-middleware.ts`.

### 5.4 go-github (Go)

```go
// github/messages.go
SHA256SignatureHeader = "X-Hub-Signature-256"
EventTypeHeader       = "X-Github-Event"     // canonical Go form; GitHub writes X-GitHub-Event
DeliveryIDHeader      = "X-Github-Delivery"

func ValidatePayload(r *http.Request, secretToken []byte) (payload []byte, err error) {
    signature := r.Header.Get(SHA256SignatureHeader)
    if signature == "" { signature = r.Header.Get(SHA1SignatureHeader) }
    contentType, _, err := mime.ParseMediaType(r.Header.Get("Content-Type"))
    ...
    return ValidatePayloadFromBody(contentType, r.Body, signature, secretToken)
}
func ValidatePayloadFromBody(contentType string, readable io.Reader, signature string, secretToken []byte) ([]byte, error)
func ValidateSignature(signature string, payload, secretToken []byte) error
func WebHookType(r *http.Request) string  { return r.Header.Get(EventTypeHeader) }
func DeliveryID(r *http.Request) string   { return r.Header.Get(DeliveryIDHeader) }
```

Both shapes: request-taking (`ValidatePayload`, `WebHookType`, `DeliveryID`)
and value-taking (`ValidatePayloadFromBody`, `ValidateSignature`). Case is
handled by `net/http.Header.Get`, which canonicalises the key; the constant
`X-Github-Event` differs from GitHub's `X-GitHub-Event` in case and still
matches. An empty signature is `errors.New("missing signature")` in
`messageMAC`; a value without `=` is `error parsing signature %q`. Missing and
empty are the same because `Header.Get` returns `""` for both.

### 5.5 svix and Standard Webhooks (Python, Go)

```python
# standard-webhooks libraries/python/standardwebhooks/webhooks.py
def verify(self, data, headers: t.Dict[str, str], *, json_parse: bool = True):
    headers = {k.lower(): v for (k, v) in headers.items()}
    msg_id = headers.get("webhook-id"); msg_signature = headers.get("webhook-signature"); msg_timestamp = headers.get("webhook-timestamp")
    if not (msg_id and msg_timestamp and msg_signature):
        raise WebhookVerificationError("Missing required headers")
```

The Python libraries take a plain `dict` and **lowercase every key themselves**
(svix's wrapper does it again before delegating). This is the one surveyed
design that absorbs the case footgun inside the library; the cost is an O(n)
rebuild of the dict per verification. Go takes `http.Header` and uses `Get`
(canonicalising). Rust takes `http::HeaderMap` (section 2).

### 5.6 Standard Webhooks specification

The spec names the three headers in lowercase ("All of the headers should be
prefixed with `webhook-` and follow the exact naming as below") and says
nothing about header-name case-insensitivity, about lowercasing on receipt, or
about which container a verifier should accept. Case handling is left to each
library, and they differ (5.5).

**Sources.**
[gidgethub `gidgethub/sansio.py`](https://github.com/gidgethub/gidgethub/blob/master/gidgethub/sansio.py),
[gidgethub `docs/sansio.rst`](https://github.com/gidgethub/gidgethub/blob/master/docs/sansio.rst),
[webhooks.net `src/Octokit.Webhooks/WebhookHeaders.cs`](https://github.com/octokit/webhooks.net/blob/main/src/Octokit.Webhooks/WebhookHeaders.cs),
[webhooks.net `src/Octokit.Webhooks/WebhookEventProcessor.cs`](https://github.com/octokit/webhooks.net/blob/main/src/Octokit.Webhooks/WebhookEventProcessor.cs),
[webhooks.net `src/Octokit.Webhooks/WebhookSignatureValidator.cs`](https://github.com/octokit/webhooks.net/blob/main/src/Octokit.Webhooks/WebhookSignatureValidator.cs),
[webhooks.net `src/Octokit.Webhooks.AspNetCore/GitHubWebhookExtensions.cs`](https://github.com/octokit/webhooks.net/blob/main/src/Octokit.Webhooks.AspNetCore/GitHubWebhookExtensions.cs),
[webhooks.net `src/Octokit.Webhooks.AzureFunctions/GitHubWebhooksHttpFunction.cs`](https://github.com/octokit/webhooks.net/blob/main/src/Octokit.Webhooks.AzureFunctions/GitHubWebhooksHttpFunction.cs),
[dotnet/runtime `StringValues.cs`](https://github.com/dotnet/runtime/blob/main/src/libraries/Microsoft.Extensions.Primitives/src/StringValues.cs) (`ToString`),
[webhooks.js `src/middleware/create-middleware.ts`](https://github.com/octokit/webhooks.js/blob/main/src/middleware/create-middleware.ts),
[webhooks.js `src/middleware/node/get-request-header.ts`](https://github.com/octokit/webhooks.js/blob/main/src/middleware/node/get-request-header.ts),
[webhooks.js `src/middleware/web/get-request-header.ts`](https://github.com/octokit/webhooks.js/blob/main/src/middleware/web/get-request-header.ts),
[webhooks.js `src/verify-and-receive.ts`](https://github.com/octokit/webhooks.js/blob/main/src/verify-and-receive.ts),
[webhooks-methods.js `src/web.ts`](https://github.com/octokit/webhooks-methods.js/blob/main/src/web.ts),
[go-github `github/messages.go`](https://github.com/google/go-github/blob/master/github/messages.go),
[svix `python/svix/webhooks.py`](https://github.com/svix/svix-webhooks/blob/main/python/svix/webhooks.py),
[standard-webhooks `libraries/python/standardwebhooks/webhooks.py`](https://github.com/standard-webhooks/standard-webhooks/blob/main/libraries/python/standardwebhooks/webhooks.py),
[svix `go/webhook.go`](https://github.com/svix/svix-webhooks/blob/main/go/webhook.go),
[Standard Webhooks spec](https://github.com/standard-webhooks/standard-webhooks/blob/main/spec/standard-webhooks.md).

---

## 6. Generic header-abstraction patterns in Rust

### 6.1 The `headers` crate's typed-header pattern

```rust
// headers-core/src/lib.rs
pub trait Header {
    fn name() -> &'static HeaderName;
    fn decode<'i, I>(values: &mut I) -> Result<Self, Error> where Self: Sized, I: Iterator<Item = &'i HeaderValue>;
    fn encode<E: Extend<HeaderValue>>(&self, values: &mut E);
}
// headers/src/map_ext.rs
pub trait HeaderMapExt: Sealed {
    fn typed_insert<H: Header>(&mut self, header: H);
    fn typed_get<H: Header>(&self) -> Option<H>;
    fn typed_try_get<H: Header>(&self) -> Result<Option<H>, Error>;
}
impl HeaderMapExt for http::HeaderMap { /* get_all(H::name()).iter() then H::decode */ }
```

`Header` is defined in terms of `HeaderName` and `HeaderValue` and
`HeaderMapExt` is implemented for `http::HeaderMap` only (the trait is sealed).
Expressing `HeaderView` as six typed headers would tie the sans-I/O path *more*
tightly to `http`, not less, and would gain nothing over `HeaderMap::get(&str)`
for six single-valued string headers. Not applicable to the question.

### 6.2 Is there a shared "look a header up by name" trait?

None found. `tower-http`'s crate docs state the ecosystem's position: "All
middleware uses the [http] and [http-body] crates as the HTTP abstractions.
That means they're compatible with any library or framework that also uses
those crates." `axum-core`, `tower-http` and `http-body` define no header-lookup
trait. Every Rust runtime in section 3 converged on `http::HeaderMap` as the
interchange type. A crate-local `HeaderLookup` trait would have no upstream
implementors.

### 6.3 Case-insensitive matching without allocation

`str::eq_ignore_ascii_case` is in `std` (compiled and checked locally). The
`unicase` crate (2.9.0) wraps a string in `Ascii<S>` whose `PartialEq` calls
`eq_ignore_ascii_case`; it adds a dependency for what `std` already offers.
`http` itself uses a 256-entry lowercase table on the stack (section 4).

**Sources.**
[hyperium/headers `headers-core/src/lib.rs`](https://github.com/hyperium/headers/blob/master/headers-core/src/lib.rs),
[hyperium/headers `src/map_ext.rs`](https://github.com/hyperium/headers/blob/master/src/map_ext.rs),
[tower-http `tower-http/src/lib.rs`](https://github.com/tower-rs/tower-http/blob/main/tower-http/src/lib.rs) lines 5-10,
[seanmonstar/unicase `src/ascii.rs`](https://github.com/seanmonstar/unicase/blob/master/src/ascii.rs),
crates.io API for `unicase`; local `rustc` check of `str::eq_ignore_ascii_case`.

---

## 7. Alternative shapes, evaluated

| # | Shape | Precedent | For octoevents |
|---|---|---|---|
| 1 | `&http::HeaderMap` only, `http` non-optional | svix, standardwebhooks (Rust); every runtime in section 3 already links `http` | **+1 crate** (`bytes`, `itoa` already present), MSRV 1.57 < the crate's 1.88, `std`-only which the crate already is. Loses nothing any surveyed target needs. Removes `from_lookup`'s footgun by construction: `HeaderMap::get(&str)` is case-insensitive. Cost: a consumer who hand-parses a serverless event JSON into `HashMap<String, String>` must build a `HeaderMap` (`HeaderName::from_bytes` allocates once per name) or use the setters. The `http` *feature* would still gate the receiver's body crates. |
| 2 | Positional `&str` values | async-stripe `(payload, sig, secret)`, slack `(hash, body, ts)`, octocrab `(header, body)`, go-github `ValidateSignature(signature, payload, secret)`, .NET `Verify(signatureHeader, secret, body)` | These take one to three values. `HeaderView` carries six optional strings; six positional `Option<&str>`s is the shape the current docs reject. No transposition incident was found in the sources, but nothing in them argues for six positional strings either. The one-value case is different: a `Verifier::verify(signature: &str, body: &[u8])` already exists and is the value-taking seam every library has. |
| 3 | Named-fields struct of values | Octokit.Webhooks `WebhookHeaders` (7 `string?` properties), `@octokit/webhooks` `{ id, name, payload, signature }` | This is `HeaderView`. The two precedents differ from it in two ways worth noting: they hold owned strings (no lifetime), and neither is a builder; `WebhookHeaders` is `init`-only properties, the JS one an object literal. Neither has a malformed flag. |
| 4 | Closure or mapping lookup | gidgethub `Mapping[str, str]` ("expected to support lowercase keys"), svix/standardwebhooks Python `Dict[str, str]` (library lowercases), `@octokit/webhooks` `getRequestHeader(request, key)` injected per platform, .NET `IDictionary<string, StringValues>` (adapter supplies `OrdinalIgnoreCase`) | This is `from_lookup`. Of the four precedents, one (Python standardwebhooks) normalises inside the library; three push it to the container and ship, or document, adapters that satisfy it. None of them documents the failure symptom; octoevents does. None passes a canonical mixed-case key (only .NET does, relying on the comparer). |
| 5 | `from_pairs(iter)` with `eq_ignore_ascii_case` inside | Python standardwebhooks (rebuilds the dict lowercased, then looks up); `http`'s own `find` (table-lowercases the probe, compares against stored lowercase); Go `Header.Get` canonicalises the probe | Removes the footgun for a string map at the cost of scanning every header (GitHub sends roughly 15-25) and comparing each against six constants: about 100-150 `eq_ignore_ascii_case` calls of short strings, no allocation. Precedent for the *mechanism* is strong (that is what `HeaderMap` and Go do); precedent for the *API shape* (an iterator of pairs) was not found in any webhook library. Compatible with `worker::Headers::entries()`, `HashMap::iter()`, and `HeaderMap::iter()`. |
| 6 | `trait HeaderLookup { fn get(&self, name: &str) -> Option<&str> }` | None in the Rust ecosystem (6.2). `@octokit/webhooks`' injected `getRequestHeader` is the closest, and it is an internal seam, not a public trait | Would be implemented only by this crate for `HeaderMap`, `HashMap`, `BTreeMap`; string-map impls would carry the same case caveat as `from_lookup`, so it moves the footgun rather than removing it. `from_lookup` is this trait with the impl inlined at the call site. |

### Malformed versus missing

Only the two Rust `HeaderMap`-taking libraries (svix, standardwebhooks) tell a
signature header that is present but not visible ASCII apart from an absent one
(`InvalidHeader` vs `MissingHeader`). Every string-based API (gidgethub, .NET,
JS, Go, stripe, slack) cannot, because a string cannot hold the malformed bytes;
Go and .NET further collapse missing into `""`. `axum-github-webhook-extract`
has the bytes and still collapses (`to_str().ok()` then `"signature missing"`).
octoevents' `malformed_signature` flag is therefore in the company of svix and
standardwebhooks, and ahead of the GitHub-specific crates. Whether the
distinction earns a `bool` on the struct, as opposed to a single `Option<Result<Cow<str>, Malformed>>`-shaped
field, is a representation question the precedents do not settle; svix folds it
into the error type at lookup time.

### Redacting the signature or secret in `Debug`

svix and standardwebhooks redact the **key** (`Webhook { key: [redacted] }`).
No surveyed library redacts the **signature header** in a debug
representation; `slack-morphism` goes the other way and puts `received_hash`
and `generated_hash` in its error's `Display`. `spin-sdk`'s `HeaderValue` and
`worker::Headers` derive or hand-write `Debug` that prints values. octoevents
is alone in redacting the signature on the header view; the argument for it (a
signature is a MAC over the body under the secret, and logging it beside the
body is a replay aid) is sound but has no precedent to point at.

---

## 8. What transfers to octoevents

Recommendations, ordered by confidence.

1. **Keep the named-fields struct and the `From<&http::HeaderMap>`
   conversion; keep `malformed_signature`; keep the redacting `Debug`.** The
   struct shape has the two closest analogues (Octokit.Webhooks
   `WebhookHeaders`, `@octokit/webhooks`' `{ id, name, payload, signature }`).
   The malformed flag matches the two most careful Rust verifiers (svix,
   standardwebhooks) and exceeds every GitHub-specific crate. The redaction has
   no precedent but no counter-argument either. Nothing found here argues for
   changing these.

2. **Make the `http` crate a non-optional dependency and keep the `http`
   feature for the receiver's body crates only.** Section 3 found no Rust
   deployment target without `http::HeaderMap`; section 4 found the crate adds
   one entry to `cargo tree` (its two dependencies are already present), needs
   MSRV 1.57 against the crate's 1.88, and is `std`-only like the crate. Its
   wasm cost is bounded at about 22 KB in isolation and is near zero on any
   runtime that links `HeaderMap` already, which is all of them. This lets
   `From<&HeaderMap>` be unconditional, which removes the only path on which a
   consumer *must* match case themselves. The persona's "string map" premise
   describes a hand-rolled Lambda event parser, not `lambda_http`; for that
   path the setters remain. If the maintainers prefer to keep `http` optional
   for principle's sake, the measurement should be recorded beside the
   decision so the trade is visible.

3. **Either drop `from_lookup` or replace it with a pairs constructor that
   matches names itself.** `from_lookup` is the one constructor whose misuse
   produces 401-on-every-delivery with the secret correct, and the persona's
   baseline reports the docs warn three times without naming the symptom
   (since fixed in the docs, but the shape still invites it). Two clean
   options, each with precedent for its mechanism:
   - *Drop it.* With `http` non-optional, a consumer holding a string map
     writes `HeaderView::new().signature(map.get(..)?)...` or builds a
     `HeaderMap`. gidgethub and .NET rely on the container; both ship or point
     at an adapter that makes it case-insensitive, which is what
     `From<&HeaderMap>` is.
   - *Replace with `from_pairs(impl IntoIterator<Item = (impl AsRef<str>, S)>)`*
     that compares each name with `eq_ignore_ascii_case` against the six
     constants. This is the Python standardwebhooks stance (the library
     absorbs case) with `http`'s and Go's mechanism (compare, do not rebuild),
     no allocation, and it accepts `worker::Headers::entries()`,
     `HashMap::iter()` and `HeaderMap::iter()` alike. The cost is a linear
     scan of 15-25 headers per delivery, which is negligible next to the HMAC.
     If kept, `from_lookup` should at minimum stop being the constructor the
     `header` module's docs lead with.

4. **Do not introduce a `HeaderLookup` trait or typed headers.** No upstream
   crate would implement the trait (6.2), and the `headers` crate's `Header`
   trait is `http`-typed through and through (6.1). Both would add surface
   without removing the footgun.

5. **Do not add positional value parameters to `from_signed`.** The
   value-taking seam every library has is the one-signature verifier, which
   `Verifier::verify(signature, body)` already is. Six positional optional
   strings has no precedent; the largest positional value API found takes
   three.

6. **Consider `Option<&str>` semantics for missing headers over the .NET and
   Go collapse.** Both `WebhookHeaders` (`""` via `StringValues.ToString()`)
   and go-github (`Header.Get` returns `""`) lose the missing-versus-empty
   distinction; `HeaderView`'s `Option` keeps it. Keep it; it costs nothing.

7. **Record the runtime survey (section 3) in `docs/design/deliberately-left-out.md`
   or beside the `http` feature's docs.** The claim "a transport that is not
   the `http` crate" is currently asserted in the persona and in the `header`
   module docs; the evidence here is that no surveyed Rust runtime is such a
   transport. Whichever way recommendation 2 goes, the docs should say what a
   non-`http` transport concretely is (a hand-parsed invocation event), so the
   sans-I/O constructor is documented for the case it actually serves.
