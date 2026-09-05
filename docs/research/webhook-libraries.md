# GitHub-webhook receivers in other ecosystems

**Question.** How do the established GitHub-webhook-receiving libraries in
other ecosystems model verification, metadata extraction, routing by kind and
action, catch-all and fallback tiers, error handling, and testing, and which
of those choices transfer to `octoevents`?

**Date.** 2026-09-05.

**Method.** Primary sources only: READMEs, source files, and official docs as
fetched on the date above. Where README and code differ, the code is
reported. Each section ends with its sources.

**Companion.** [rust-dispatch-designs.md](./rust-dispatch-designs.md)
surveys generic dispatcher designs in Rust.

---

## 1. `@octokit/webhooks` (JavaScript)

### API surface

```js
new Webhooks({ secret /*, transform, additionalSecrets, log */ });   // secret REQUIRED: throws "options.secret required"

webhooks.on(eventName, handler);        // "issues" | "issues.opened"
webhooks.on(eventNames, handler);       // array
webhooks.onAny(handler);                // stored under hooks["*"]
webhooks.onError(handler);              // stored under hooks["error"]
webhooks.removeListener(eventName(s), handler);

webhooks.receive({ id, name, payload });                    // payload: parsed object
webhooks.verifyAndReceive({ id, name, payload, signature }); // payload: raw string
webhooks.sign(eventPayload); webhooks.verify(eventPayload, signature);

createNodeMiddleware(webhooks, { path = "/api/github/webhooks", log, timeout = 9000 });
emitterEventNames; // ["check_run", "check_run.completed", ...] as const
validateEventName("push", { onUnknownEventName: "throw" | "warn" | "ignore" });
```

`EmitterWebhookEvent` (`src/types.ts`) carries exactly three fields:

```ts
interface BaseWebhookEvent<TName extends WebhookEventName> {
  id: string;
  name: TName;
  payload: EventPayloadMap[TName];
}
export type EmitterWebhookEvent<TEmitterEvent extends EmitterWebhookEventName = EmitterWebhookEventName> =
  TEmitterEvent extends `${infer TWebhookEvent}.${infer TAction}`
    ? BaseWebhookEvent<Extract<TWebhookEvent, WebhookEventName>> & { payload: { action: TAction } }
    : BaseWebhookEvent<Extract<TEmitterEvent, WebhookEventName>>;
```

No installation id, repo, or sender is lifted out; everything beyond
`id`/`name` lives in `payload`.

### Behaviour, from `src/event-handler/receive.ts`

**Hook selection** — action-specific first, then kind-wide, then `*`:

```ts
const hooks = [state.hooks[eventName], state.hooks["*"]];
if (eventPayloadAction) hooks.unshift(state.hooks[`${eventName}.${eventPayloadAction}`]);
```

The action is read from `event.payload.action` — a payload probe.

**Unmatched** → `if (hooks.length === 0) return Promise.resolve();` — silent
success.

**Run-all, parallel, aggregate errors:**

```ts
const errors: WebhookError[] = [];
const promises = hooks.map((handler) => {
  let promise = Promise.resolve(event);
  if (state.transform) promise = promise.then(state.transform);   // NB: transform runs once PER HANDLER
  return promise.then((event) => handler(event))
                .catch((error) => errors.push(Object.assign(error, { event })));
});
return Promise.all(promises).then(() => {
  if (errors.length === 0) return;
  const error = new AggregateError(errors, errors.map((e) => e.message).join("\n"));
  Object.assign(error, { event });
  errorHandlers.forEach((handler) => wrapErrorHandler(handler, error));
  throw error;
});
```

Every matching handler runs even if one rejects; each error gets the `event`
attached; the caller gets one `AggregateError`. `onError` handlers are invoked
fire-and-forget *before* the throw; if an error handler itself throws,
`wrap-error-handler.ts` prints `FATAL: Error occurred in "error" event
handler` and swallows it.

**Signature failure also flows through `onError`** — `verify-and-receive.ts`
builds an `Error` with `.event` and `.status = 400` and hands it to
`eventHandler.receive(error)`. Verification uses
`verifyWithFallback(secret, payload, signature, additionalSecrets)` — secret
rotation is built in.

**Middleware** (`src/middleware/create-middleware.ts`): path match → POST only
(404) → `Content-Type` must match `/^\s*(application\/json)\s*(?:;|$)/` (415)
→ required headers `x-github-event`, `x-hub-signature-256`,
`x-github-delivery` (400) → then:

```ts
// GitHub will abort the request if it does not receive a response within 10s
// See https://github.com/octokit/webhooks.js/issues/185
const timeoutPromise = new Promise((resolve) => {
  timeout = setTimeout(() => { didTimeout = true;
    resolve(handleResponse("still processing\n", 202, ...)); }, options.timeout);  // 9000 ms
});
...
return await Promise.race([timeoutPromise, processWebhook()]);
```

On error the status is `errors[0].status ?? 500`; on success `200 "ok\n"`.

**Name generation** (`scripts/generate-types.ts`): from
`@octokit/openapi-webhooks`, each `webhooks[key].post.operationId` (e.g.
`issues/opened`) becomes `operationId.replace(/-/g,"_").replace("/", ".")`;
both the bare kind and the dotted form are added. `on()` calls
`validateEventName(name, { onUnknownEventName: "warn" })`.

### Portable ideas

| Idea | Cost in `octoevents` |
|---|---|
| **`on_error` observer**: a hook that sees `(&EventMeta, &E)` on any failed dispatch, distinct from result propagation. | Low. Partly redundant with `tracing`, but metrics/alerting users without tracing want it. |
| **Error carries the HTTP status.** | Low. GitHub doesn't auto-retry on 5xx, so value is mostly the delivery UI. |
| **9-second race → `202 still processing`.** | Medium. Sans-I/O can't own a timer; a runtime-agnostic `Spawned<H>` adapter or documentation. |
| **Run-all-and-aggregate** instead of fail-fast. | Medium-high. Parallel execution fights `MaybeSend`. Ecosystem is split (§8). |
| `"issues.opened"` as a parse/display form. | Low. Config-driven routes, tracing/metrics label. |

Sources: [README](https://github.com/octokit/webhooks.js/blob/main/README.md),
[`src/event-handler/receive.ts`](https://github.com/octokit/webhooks.js/blob/main/src/event-handler/receive.ts),
[`src/event-handler/on.ts`](https://github.com/octokit/webhooks.js/blob/main/src/event-handler/on.ts),
[`src/event-handler/wrap-error-handler.ts`](https://github.com/octokit/webhooks.js/blob/main/src/event-handler/wrap-error-handler.ts),
[`src/verify-and-receive.ts`](https://github.com/octokit/webhooks.js/blob/main/src/verify-and-receive.ts),
[`src/types.ts`](https://github.com/octokit/webhooks.js/blob/main/src/types.ts),
[`src/middleware/create-middleware.ts`](https://github.com/octokit/webhooks.js/blob/main/src/middleware/create-middleware.ts),
[`scripts/generate-types.ts`](https://github.com/octokit/webhooks.js/blob/main/scripts/generate-types.ts).

---

## 2. Probot (JavaScript)

Probot is a thin layer over `@octokit/webhooks`: `app.on/onAny/onError` are
forwarded verbatim (`src/probot.ts`). `on()` validates names with
`onUnknownEventName: "ignore"`.

### API surface

```js
app.on("issues.opened", async (context) => { ... });
app.on(["issues.opened", "issues.edited"], async (context) => { ... });
app.onAny(async (context) => { app.log.info({ event: context.name, action: context.payload.action }); });
app.onError(handler);
await probot.receive({ name: "issues", payload });   // tests (docs/testing.md), paired with nock
```

### The `Context` object (`src/context.ts`)

```ts
export class Context<Event extends WebhookEvents = WebhookEvents> {
  public name: WebhookEvents;
  public id: string;
  public payload: {...}[Event]["payload"];
  public octokit: ProbotOctokit;             // authenticated for the installation
  public log: Logger;                        // pino child: { name: "event", id: event.id }
  repo<T>(object?: T): { owner, repo } & T          // throws if no repository in payload
  issue<T>(object?: T): repo & { issue_number }
  pullRequest<T>(object?: T): repo & { pull_number }
  get isBot(): boolean                               // payload.sender?.type === "Bot"
  async config<T>(fileName, defaultConfig?, deepMergeOptions?)
}
```

**Per-installation authentication is the `transform` hook**
(`src/octokit/octokit-webhooks-transform.ts`). `"event-octokit"` auth
(`octokit-auth-probot`) has three outcomes: token-auth → same instance;
`installation` event with `action` `suspend` or `deleted` → **unauthenticated**
Octokit; otherwise installation auth on `payload.installation.id`.

**Delivery ID usage.** No idempotency/dedup anywhere. Used for the log child
and a correlation header on every outbound API call:

```ts
// set `x-github-delivery` header on all requests sent in response to the current event.
// This allows GitHub Support to correlate the request with the event.
octokit.hook.before("request", (options) => { options.headers["x-github-delivery"] = event.id; });
```

**Error handler** (`src/helpers/get-error-handler.ts`) pattern-matches messages
containing `x-hub-signature-256` or `pem`/`json web token` to print operator
hints.

### Portable ideas

| Idea | Cost |
|---|---|
| **`EventMeta` convenience accessors**: `is_bot()`, `repo()`. | Low; one more probed field. |
| **Document the `installation.suspend/deleted` edge** for token minting. | Trivial. |
| **Delivery-ID correlation header** as a documented consumer pattern. | Zero. |
| **Operator-hint error classification.** | Low. |

Sources: [docs/webhooks.md](https://github.com/probot/probot/blob/master/docs/webhooks.md),
[docs/testing.md](https://github.com/probot/probot/blob/master/docs/testing.md),
[`src/context.ts`](https://github.com/probot/probot/blob/master/src/context.ts),
[`src/probot.ts`](https://github.com/probot/probot/blob/master/src/probot.ts),
[`src/octokit/octokit-webhooks-transform.ts`](https://github.com/probot/probot/blob/master/src/octokit/octokit-webhooks-transform.ts),
[`src/helpers/get-error-handler.ts`](https://github.com/probot/probot/blob/master/src/helpers/get-error-handler.ts),
[octokit-auth-probot README](https://github.com/probot/octokit-auth-probot).

---

## 3. `go-playground/webhooks` (Go)

```go
hook, _ := github.New(github.Options.Secret("..."))
payload, err := hook.Parse(r, github.ReleaseEvent, github.PullRequestEvent)
if err != nil {
    if err == github.ErrEventNotFound { /* ok event wasn't one of the ones asked to be parsed */ }
}
switch payload.(type) {
case github.ReleasePayload:     release := payload.(github.ReleasePayload)
case github.PullRequestPayload: pullRequest := payload.(github.PullRequestPayload)
}
```

`Parse` runs, in order: method check → `X-GitHub-Event` present →
**membership check against the passed list** (`ErrEventNotFound`) → read body
→ HMAC-SHA256 **only if a secret is set** → a ~50-arm `switch` that
`json.Unmarshal`s into the hand-written struct for that kind.

Consequences: events not in the list return `ErrEventNotFound` **before the
body is read or the signature verified**; `ping` gets no special treatment;
the secret is optional; no action routing; no delivery ID exposed.

**Limits people hit** (issue tracker): hand-written payload structs missing
fields — #200 "PullRequestPayload Struct has missing fields", #170
"RepositoryPayload is not catching event for repository edited, renamed and
transfer action", #50, #35, #11. One blessed struct per kind cannot keep up
with GitHub's per-action payload variance.

### Portable ideas

- **Negative example, already avoided**: verify before deciding whether the
  event is interesting.
- **Validation of the `impl_payload!` design**: consumer-defined serde views
  sidestep the "missing field" issue class. Worth stating in docs.
- **Typed sentinel for "kind not registered"** as a distinct outcome.

Sources: [README](https://github.com/go-playground/webhooks/blob/master/README.md),
[`github/github.go`](https://github.com/go-playground/webhooks/blob/master/github/github.go),
issues [#170](https://github.com/go-playground/webhooks/issues/170),
[#200](https://github.com/go-playground/webhooks/issues/200).

---

## 4. `google/go-github` webhook helpers (Go)

```go
payload, err := github.ValidatePayload(r, s.webhookSecretKey)      // []byte
event,   err := github.ParseWebHook(github.WebHookType(r), payload) // any
switch event := event.(type) {
case *github.CommitCommentEvent: processCommitCommentEvent(event)
}
github.WebHookType(r)  // r.Header.Get("X-Github-Event")
github.DeliveryID(r)   // r.Header.Get("X-Github-Delivery")
github.ValidateSignature(signature, payload, secretToken) error
github.MessageTypes() []string; github.EventForType(t) any
```

- `ValidatePayload` reads `X-Hub-Signature-256`, **falls back to
  `X-Hub-Signature` (SHA-1)**; accepts `application/json` *and*
  `application/x-www-form-urlencoded` (JSON under the `payload` form param —
  the signature is over the raw form body); enforces a **25 MB** cap via
  `io.LimitReader`; verifies only `if len(secretToken) > 0 || len(signature) > 0`.
- `ParseWebHook` looks the name up in a `map[string]any` of prototypes.

No routing; three orthogonal, individually testable functions with byte
slices between them.

### Portable ideas

- **Expose the three stages as separate public functions** (verify / read
  kind / decode).
- **`MessageTypes()`-style introspection**: a `const` slice of known
  `EventKind`s.
- Counter-example: SHA-1 fallback and form-urlencoded support. Keep rejecting.

Source: [`github/messages.go`](https://github.com/google/go-github/blob/master/github/messages.go).

---

## 5. gidgethub (Python, sans-I/O)

The closest philosophical match. Module docstring: *"This code has been
constructed to perform no I/O of its own."*

```python
# gidgethub/sansio.py
class Event:
    def __init__(self, data: Any, *, event: str, delivery_id: str): ...
    @classmethod
    def from_http(cls, headers: Mapping[str, str], body: bytes, *, secret: Optional[str] = None) -> "Event": ...
def validate_event(payload: bytes, *, signature: str, secret: str) -> None   # raises ValidationFailure

# gidgethub/routing.py
class Router:
    def __init__(self, *other_routers: "Router"): ...
    def add(self, func, event_type: str, **data_detail: Any) -> None: ...
    def register(self, event_type: str, **data_detail): ...          # decorator
    def fetch(self, event) -> FrozenSet[AsyncCallback]: ...
    async def dispatch(self, event, *args, **kwargs) -> None: ...

@router.register("pull_request", action="opened")
async def opened_pr(event, gh, *arg, **kwargs):
    await gh.post(event.data["pull_request"]["labels_url"], data=["needs review"])
```

**`Event` is three fields**: `data`, `event` (raw name string), `delivery_id`.
The name is deliberately not an enum:

```python
# Event is not an enum as GitHub provides the string. This allows them
# to add new events without having to mirror them here. There's also no
# direct worry of a user typing in the wrong event name and thus no need
# for an enum's typing protection.
```

**`from_http` verification is symmetric and strict**: signature header present
but `secret=None` → `ValidationFailure("secret not provided")`; secret given
but no signature → `ValidationFailure("signature is missing")`; both absent →
proceeds unverified.

**Routing key is generic.** `add(func, event_type, **data_detail)` accepts
**zero or one** keyword, matched against **any top-level key** of
`event.data`. Two keywords → `TypeError("dispatching based on data details is
only supported up to one level deep")`.

```python
self._shallow_routes: Dict[str, List[AsyncCallback]]
self._deep_routes:    Dict[str, Dict[str, Dict[Any, List[AsyncCallback]]]]
```

**Both tiers fire**: `fetch()` unions shallow and deep matches into a
`frozenset`.

**Dispatch is sequential and fail-fast**, extra args threaded through:

```python
async def dispatch(self, event, *args, **kwargs) -> None:
    found_callbacks = self.fetch(event)
    for callback in found_callbacks:
        await callback(event, *args, **kwargs)
```

**Order is non-deterministic** (docs: *"versionchanged 5.0.0: Execution order
is non-deterministic"*) because `fetch` returns a set.

**Unmatched event → nothing happens, no error.**

**Composition** `Router(a, b)` flattens by re-`add`ing every route — copy
semantics, not delegation.

**Extra args** are how dependencies reach handlers; "a handler is a struct
whose fields are its dependencies" is the Rust-native answer to the same need.

### Portable ideas

| Idea | Cost |
|---|---|
| **`fetch(event)` / dry-run introspection**: which routes would this envelope hit? | Low. |
| **Router composition** `DispatcherBuilder::merge(other)`. Nesting a dispatcher as a handler makes the inner unmatched-success look like a match to the outer tier. | Low for same-`E`. |
| **Ping as a first-class pre-dispatch concern.** | Doc + maybe a canned handler. |
| Counter-example: generic `key=value` matching beyond `action`. | Stringly-typed; typed `(EventKind, Option<Action>)` is the right trade. |

Sources: [`gidgethub/routing.py`](https://github.com/gidgethub/gidgethub/blob/master/gidgethub/routing.py),
[`gidgethub/sansio.py`](https://github.com/gidgethub/gidgethub/blob/master/gidgethub/sansio.py),
[docs/routing.rst](https://github.com/gidgethub/gidgethub/blob/master/docs/routing.rst),
[docs/sansio.rst](https://github.com/gidgethub/gidgethub/blob/master/docs/sansio.rst).

---

## 6. Symfony EventDispatcher (PHP) — transferable ideas only

```php
$dispatcher->addListener('kernel.exception', $listener, $priority = 0);
$event->stopPropagation();  $event->isPropagationStopped();
class ExceptionSubscriber implements EventSubscriberInterface {
    public static function getSubscribedEvents(): array {
        return [ExceptionEvent::class => [['processException', 10], ['logException', 0], ['notifyException', -10]]];
    }
}
new TraceableEventDispatcher($dispatcher, new Stopwatch());  // getCalledListeners() / getNotCalledListeners()
new ImmutableEventDispatcher($dispatcher);
```

1. **Subscriber = a type that declares its own subscriptions.** Docs:
   *"Subscribers are easier to reuse because the knowledge of the events is
   kept in the class rather than in the service definition."*
2. **Immutable-after-build.** Builder → `Dispatcher` already does this.
3. **Traceable dispatcher** (called vs not-called listeners).
4. **Not worth porting**: numeric priorities, `stopPropagation`, the mutable
   `Event` object.

Source: [Symfony docs](https://symfony.com/doc/current/components/event_dispatcher.html).

---

## 7. Rust crates

`octokit-webhooks` (Rust) and `gh-webhook` do not exist as crates. The real
ones:

| Crate | Version / last release | Maintained? | What it does | Routing? |
|---|---|---|---|---|
| `octocrab` (`models::webhook_events`) | active | yes | parse header+body into `WebhookEvent` | no |
| `github-webhook` (sksat) | 0.5.2 / 2024-01 | dormant | payload types generated from octokit `schema.d.ts` | no |
| `axum-github-webhook-extract` | 0.3.0 / 2025-03 | yes | Axum extractor: verify + `serde` into `T` | no |
| `tower-github-webhook` | 0.2.0 / 2024-08 | slow | Tower `Layer`/`Service` that verifies the signature | no |
| `github_webhook_message_validator` | 0.1.6 / 2022-07 | dead | `validate(secret, sig, msg) -> bool`, **HMAC-SHA1 only** | no |
| `octoapp` | 0.5.1 / 2026-08 | active | GitHub App framework (Rocket/Hyper) | single closure + `match` |
| `octofer` | 0.1.0 / 2025-10 | "under development" | Probot-inspired App framework on Axum | by kind only |
| `tide-github` (paritytech) | 0.3.0 / 2022-03 | dead | tide sub-app | one handler per kind |
| `afterparty` (softprops) | 0.2.0 / 2017 | dead | hyper `Hub` of `Hook`s | by name + `"*"` |
| `warp_github_webhook` | 0.7.0 / 2022-01 | dead | warp filter | no |

### `octocrab::models::webhook_events`

```rust
pub struct WebhookEvent {
    pub sender: Option<Author>,
    pub repository: Option<Repository>,
    pub organization: Option<Organization>,
    pub installation: Option<EventInstallation>,   // Full(Box<Installation>) | Minimal(Box<EventInstallationId>), .id()
    #[serde(skip)] pub kind: WebhookEventType,
    #[serde(flatten)] pub specific: WebhookEventPayload,
}
impl WebhookEvent {
    pub fn try_from_header_and_body<B: AsRef<[u8]> + ?Sized>(header: &str, body: &B) -> Result<Self, serde_json::Error>
}
#[non_exhaustive] #[serde(rename_all = "snake_case")]
pub enum WebhookEventType { BranchProtectionRule, CheckRun, ..., WorkflowRun,
    #[serde(untagged)] Unknown(String) }
```

The header is parsed by wrapping it in quotes and
`serde_json::from_str::<WebhookEventType>`; the body is deserialised into an
`Intermediate` struct that pulls out common fields and flattens the rest into
a `serde_json::Value`, then a second `from_value` into a
`Box<XxxWebhookEventPayload>`. Two passes; every variant boxed. Module docs:
*"you should consider octocrab's support for webhooks in beta state."*

### The rest, briefly

- **`axum-github-webhook-extract`**: `GithubToken(pub Arc<String>)` in state +
  `GithubEvent<T>(pub T)` extractor. Ignores `X-GitHub-Event` entirely.
- **`tower-github-webhook`**: verification as middleware, stops there.
- **`octoapp`**: `#[serde(untagged)] pub enum Event { Issues(IssuesEvent), ... }`
  — the kind is inferred from payload shape, not the header; ambiguous shapes
  can mis-resolve. Single `on_event` closure.
- **`octofer`**: one `on_<kind>()` method per kind (~70),
  `HashMap<kind, Vec<EventHandlerFn>>`, sequential fail-fast → 500,
  unmatched → 200. No action routing. Dependencies via `extra: Arc<T>`.
- **`tide-github`**: `HashMap<Event, Arc<dyn Fn(Payload)>>`, so a second
  `.on()` for the same kind **overwrites** the first; unknown kind → `501`,
  kind with no handler → `501 MissingHandlerForEvent` (the only strict one).
- **`afterparty`** (2016): `Hub::handle("*", ...)`, per-hook secrets,
  `Delivery { id, event, payload: Event }`.

### Portable ideas from the Rust set

- **An Axum extractor** as a thin feature, for people who won't adopt a
  `WebhookHandler`.
- **Normalise the installation object** (octocrab's `EventInstallation::id()`).
- **Negative example**: `octoapp`'s untagged enum shows why the kind must
  come from the header.
- **Negative example**: `tide-github`'s overwrite-on-duplicate and
  501-on-unmatched are the two behaviours nobody else chose.

Sources: [octocrab `webhook_events.rs`](https://github.com/XAMPPRocky/octocrab/blob/main/src/models/webhook_events.rs),
[sksat/github-webhook-rs](https://github.com/sksat/github-webhook-rs),
[axum-github-webhook-extract](https://github.com/daaku/axum-github-webhook-extract/blob/main/src/lib.rs),
[tower-github-webhook](https://github.com/SebRollen/tower-github-webhook/blob/main/src/lib.rs),
[github_webhook_message_validator](https://github.com/qubyte/github_webhook_message_validator/blob/master/src/lib.rs),
[octoapp `events/mod.rs`](https://github.com/42ByteLabs/octoapp/blob/main/src/events/mod.rs),
[octofer `webhook/handlers.rs`](https://github.com/AbelHristodor/octofer/blob/main/src/webhook/handlers.rs),
[tide-github `lib.rs`](https://github.com/paritytech/tide-github/blob/master/src/lib.rs),
[afterparty](https://github.com/softprops/afterparty).

---

## 8. Synthesis

### What the ecosystem has converged on

| Concern | Convergence | `octoevents` (0.1) |
|---|---|---|
| **Both `kind` and `kind.action` routes fire**, action-specific first | octokit, gidgethub, Probot | same |
| **Dotted `issues.opened` name** | octokit, Probot; gidgethub uses `("issues", action="opened")`; no Rust crate routes by action | typed tuple; no string form |
| **`onAny` / `"*"`** | octokit, Probot, afterparty | `always` — stronger (ordered first, failure fails the delivery, never a match) |
| **Unmatched delivery is a silent success** | octokit, Probot, gidgethub, octofer, go-github | default, opt-in `fallback`. Only tide-github and go-playground chose strict |
| **A test path that bypasses HTTP and verification** | octokit/Probot `receive`, gidgethub `Event(...)`, go-github `ParseWebHook` | `Envelope { meta, raw }` hand-construction |
| **Secret is mandatory** | octokit, octoevents | go-playground, go-github, gidgethub, afterparty allow unverified |
| **Secret rotation** | octokit `additionalSecrets` | `Verifier::also` |
| **Forward-compat unknown kind** | octocrab `Unknown(String)`, gidgethub plain string | `EventKind::Unknown`, `Action::Unknown` |
| **Kind from header, not body shape** | everyone except `octoapp` | yes |
| **Verify before anything else** | octokit, go-github, gidgethub | yes |
| **Payload size bound** | go-github (25 MB) | yes |
| **`onError` observer separate from the result** | octokit, Probot | no (partially `tracing`) |
| **Run-all vs fail-fast** | **Not converged.** JS: parallel, aggregate. Python, Rust, PHP: sequential fail-fast | sequential fail-fast |
| **10-second response deadline** | octokit (9 s race → 202); tide-github (spawn + return) | not addressed |
| **Installation-scoped API client per event** | Probot `transform`, octoapp, octofer | out of scope by design |

### Ranked: the most transferable ideas

1. **`on_error` observer** (octokit, Probot). The one converged feature the
   crate lacks.
2. **Public, explicitly-unverified `Envelope` constructor for tests** plus a
   fixture helper.
3. **Route introspection / dry-run** (gidgethub `fetch`, Symfony
   `TraceableEventDispatcher`, octokit `emitterEventNames`).
4. **`DispatcherBuilder::merge` + a `Subscribe` trait** (gidgethub, Symfony).
5. **Deadline story** (octokit 9 s/202, GitHub's 10 s abort).
6. **`EventMeta` accessors**: `is_bot()`, `repo()`,
   `installation_access_revoked()`.
7. **`FromStr`/`Display` for the dotted route form.**
8. **Error → response status mapping.**
9. **Run-all-and-aggregate as an opt-in policy**, not the default.
10. **Document the deliberate rejections with their evidence**: no SHA-1
    fallback, no form-urlencoded, no untagged-enum kind inference, no
    one-blessed-struct-per-kind, no numeric priorities or `stopPropagation`.
