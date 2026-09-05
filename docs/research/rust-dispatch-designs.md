# Dispatcher designs in the Rust ecosystem

**Question.** How do established Rust libraries model handler registration,
routing, error handling, and composition, and which of those ideas transfer
to a small, typed, sans-I/O webhook dispatcher that must compile on wasm32?

**Date.** 2026-09-05.

**Method.** Primary sources only: docs.rs pages and GitHub source as served on
the date above. Versions: dptree 0.5.1 (+ README on master), teloxide 0.17.0,
axum 0.8.9 / axum-core 0.5.6 / axum-macros 0.5.1, tower 0.5.3, serenity
0.12.5, twilight-gateway 0.17.1 / twilight-model 0.17.1, poise 0.6.2,
std 1.98. Signatures are quoted from those pages.

**Companion.** [webhook-libraries.md](./webhook-libraries.md) surveys the
GitHub-webhook-receiving libraries in other ecosystems.

---

## 1. dptree

Source: <https://docs.rs/dptree/0.5.1/dptree/>,
<https://github.com/teloxide/dptree>.

### The `Handler` type and CPS

```rust
pub struct Handler<'a, Output, Descr = description::Unspecified> {
    data: Arc<HandlerData<Descr, DynFn<'a, Output>>>,
}
struct HandlerData<Descr, F: ?Sized> { description: Descr, sig: HandlerSignature, f: F }

type DynFn<'a, Output> =
    dyn Fn(DependencyMap, Cont<'a, Output>) -> HandlerResult<'a, Output> + Send + Sync + 'a;
pub type Cont<'a, Output> =
    Box<dyn FnOnce(DependencyMap) -> HandlerResult<'a, Output> + Send + Sync + 'a>;
pub type HandlerResult<'a, Output> = BoxFuture<'a, ControlFlow<Output, DependencyMap>>;
```

(`dptree/handler/core.rs` lines 62–131.)

Every handler is a function of `(input container, continuation)`. It either
calls `cont(input)` (pass on) or doesn't (terminate). Everything is boxed: the
handler fn is `dyn Fn`, the continuation is `Box<dyn FnOnce>`, the result is
`BoxFuture`. `Handler` is `Clone` via `Arc`. `Send + Sync` are required
throughout.

```rust
pub fn from_fn<'a, F, Fut, Output, Descr>(f: F, sig: HandlerSignature) -> Handler<'a, Output, Descr>
where
    F: Fn(DependencyMap, Cont<'a, Output>) -> Fut, F: Send + Sync + 'a,
    Fut: Future<Output = ControlFlow<Output, DependencyMap>> + Send + 'a,
    Descr: HandlerDescription,

pub async fn execute<Cont, ContFut>(self, input: DependencyMap, cont: Cont) -> ControlFlow<Output, DependencyMap>
where Cont: FnOnce(DependencyMap) -> ContFut + Send + Sync + 'a,
      ContFut: Future<Output = ControlFlow<Output, DependencyMap>> + Send + 'a,

pub async fn dispatch(&self, input: DependencyMap) -> ControlFlow<Output, DependencyMap>
```

`dispatch` is `execute(input, |event| async move { ControlFlow::Continue(event) })`
— the empty continuation. `entry()` is
`from_fn_with_description(Descr::entry(), |event, cont| cont(event), HandlerSignature::Entry)`.

### Combinators

- `entry()` — identity; only used as a root to hang `.branch` calls on.
- `filter(pred)` — "`pred` has an access to all values that are stored in the
  input container. If it returns `true`, a continuation of the handler will be
  called, otherwise the handler returns `ControlFlow::Continue`." Sync closures
  are lifted through `Asyncify<Pred>`; `filter_async` takes an async predicate.
- `filter_map(proj)` — "optionally passes a value of a new type further":
  `Some(v)` inserts `v` into the container and calls `cont`; `None` returns
  `Continue(input)`.
- `map(proj)` — always inserts the new value and continues.
- `inspect(f)` — "Like `map` but does not add return value of `f` to the container."
- `endpoint(f)` — "Constructs a handler that has no further handlers in a
  chain." Returns `Break(f(...).await)`, never calls `cont`.
- Every combinator has a `*_with_description` twin. The README on master also
  lists `try_filter`/`try_map`/`try_filter_map` ("short-circuit the chain upon
  the handler's error"); they are not in the 0.5.1 item index.

### `.chain` vs `.branch` (verbatim from the `Handler` docs)

> In `a.chain(b).c`, the handler `a` is given the rest of the handler chain,
> `b` and `c`; if `a` decides to pass the value further, it invokes `b`. Then,
> if `b` decides to pass the value further, it invokes `c`. Thus, the handler
> chain is *linear*.
>
> In `a.branch(b).c`, if `a` decides to pass the value further, it invokes `b`.
> But since `b` is "branched", it receives an empty chain, so it cannot invoke
> `c`. Instead, if `b` decides to continue execution (`ControlFlow::Continue`),
> `a` invokes `c`; otherwise (`ControlFlow::Break`), the process is terminated.
> The handler chain is *nested*.
>
> This is very crucial when `b` is a filter: if it is chained, it decides
> whether or not to call `c`, but when it is branched, whether `c` is called
> depends solely on `a`.

The two implementations differ by one line each (core.rs 222–231 and 357–371):

```rust
// chain: b gets the real continuation
this.execute(event, |event| next.execute(event, cont))
// branch: b gets an empty continuation; Continue falls through to cont
this.execute(event, |event| async move {
    match next.dispatch(event).await {
        ControlFlow::Continue(event) => cont(event).await,
        done => done,
    }
})
```

`branch` additionally requires `Output: Send`. Both are `#[track_caller]` and
panic at *construction* if misused: `"Ill-typed handler chain: the second
handler cannot be an entry"` and, for chain only, `"Dead code detected: since
the first handler aborts execution, the second handler will never be called."`
(checked via the `continues` flag in `HandlerSignature`).

### `ControlFlow<Output, DependencyMap>`

- `Break(Output)` — an endpoint ran; `dispatch` docs: "Returns
  `ControlFlow::Break` when executed successfully, `ControlFlow::Continue`
  otherwise."
- `Continue(DependencyMap)` — nothing terminated; the *input container is
  handed back* so the caller (or an enclosing `branch`) can try the next
  sibling without cloning up front.

So "handled?" and "succeeded?" are separate axes: teloxide instantiates
`Output = Result<(), Err>`, giving three outcomes (`Break(Ok)`, `Break(Err)`,
`Continue`).

### `DependencyMap`, `deps!`, `Injectable`, `type_check`

```rust
pub struct DependencyMap { /* TypeId -> Arc<dyn Any + Send + Sync> */ }
pub fn insert<T: Send + Sync + 'static>(&mut self, item: T) -> Option<Arc<T>>
pub fn insert_container(&mut self, container: Self)
pub fn remove<T: Send + Sync + 'static>(&mut self) -> Option<Arc<T>>
pub fn get<V>(&self) -> Arc<V>   where V: Send + Sync + 'static   // Panics if absent
pub fn try_get<V: Send + Sync + 'static>(&self) -> Option<Arc<V>>
```

```rust
pub trait Injectable<Output, FnArgs> where Output: 'static {
    fn inject<'a>(&'a self, container: &'a DependencyMap) -> CompiledFn<'a, Output>;
    fn input_types() -> BTreeSet<Type>;
    fn obligations() -> BTreeMap<Type, &'static Location<'static>> { ... }
}
// implemented for Fn(T1..T12) -> Fut where each Ti: Clone + Send + Sync + 'static
```

Handler arguments are looked up by `TypeId` and **cloned out of the `Arc`**
per call (hence the `Clone` bound and the README troubleshooting entry:
"Ensure that your update type implements `Clone`. If it is too expensive to
clone every single update, you can wrap it into `Arc`.").

```rust
pub fn type_check(sig: &HandlerSignature, container: &DependencyMap, assumptions: &[Type])
// Panics: "If container does not contain all the types that the handler accepts;
// in this case, helpful diagnostic information about missing types and code locations is displayed."
```

```rust
pub enum HandlerSignature {
    Entry,
    Other {
        obligations: BTreeMap<Type, &'static Location<'static>>,  // type -> where it was required
        guaranteed_outcomes: BTreeSet<Type>,
        conditional_outcomes: BTreeSet<Type>,
        continues: bool,
    },
}
pub struct Type { pub name: &'static str, pub id: TypeId }
```

`chain`/`branch` run a small inference (`infer_chain`/`infer_branch`).
`#[track_caller]` on every combinator records `Location::caller()` per
obligation, so the panic message reads e.g.

```
The missing values are:
    `...::C` from src/handler/core.rs:4:35
```

teloxide calls `type_check` in `DispatcherBuilder::build()` with assumptions
`[Type::of::<R>(), Type::of::<Update>(), Type::of::<Me>()]` (dispatcher.rs
221–229), so missing dependencies fail at startup, not on first matching
update.

### `case!`

```rust
macro_rules! case {
    ($($variant:ident)::+) => { ... };
    ($($variant:ident)::+ ($param:ident)) => { ... };
    ($($variant:ident)::+ ($($param:ident),+ $(,)?)) => { ... };
    ($($variant:ident)::+ {$param:ident}) => { ... };
    ($($variant:ident)::+ {$($param:ident),+ $(,)?}) => { ... };
}
```

Unit variant → `filter`; tuple/struct variant → `filter_map` that injects the
payload. Pure sugar over the two combinators.

### Description / introspection

```rust
pub trait HandlerDescription: Sized + Send + Sync + 'static {
    fn entry() -> Self;
    fn user_defined() -> Self;
    fn merge_chain(&self, other: &Self) -> Self;
    fn merge_branch(&self, other: &Self) -> Self;
    fn map() -> Self { ... } fn filter() -> Self { ... } fn endpoint() -> Self { ... } // etc., default = user_defined()
}
```

"This trait allows information to flow 'back up' the tree, allowing a user to
check its structure." Built-in `InterestSet<K>`:

```rust
pub struct InterestSet<K, S = RandomState> {
    pub observed: HashSet<K, S>, // "Event kinds that are of interested for a given handler. I.e. the ones that can cause meaningful side-effects."
    pub filtered: HashSet<K, S>, // "Event kinds that can be observed by handlers chained to this one."
}
```

with the caveat "the filter should not have observable side-effects".
teloxide's `DpHandlerDescription` uses this to compute `allowed_updates()` and
hint the Telegram listener (dispatcher.rs 410–413): the dispatch tree tells
the transport which events to subscribe to.

### Trade-offs the authors call out

- README "Pitfalls": "`DependencyMap` can panic at run-time if a non-existing
  dependency is requested. Use `dptree::type_check`…" and "`.branch` and
  `.chain` are different operations."
- README "Design choices / Dependency injection": "In Rust, it is possible to
  express type-safe DI that checks all types statically. However, this would
  require complex type-level manipulations, such as those in the `frunk`
  library. We decided not to trade comprehensible error messages for
  compile-time safety, since we had a plenty of experience that the
  uninitiated users simply cannot understand what is wrong with their code,
  owing to the utterly inadequate diagnostic messages from rustc."
- Implicit costs visible in source: one `BoxFuture` + one `Box<dyn FnOnce>`
  allocation per hop per delivery; `Arc` clone of every handler per call;
  `Send + Sync` everywhere (does not fit a `MaybeSend` story on
  wasm32-unknown-unknown without a fork); `colored` as a hard dependency for
  panic output.

### Portable ideas

1. **Record `Location::caller()` at registration** (`#[track_caller]` on
   `route`/`always`/`fallback`). Zero runtime cost, wasm-safe.
2. **Three-way dispatch outcome** via a `ControlFlow<Result<(), E>, _>`-shaped
   result from `dispatch` (or an `Outcome` enum). Lets the receiver/tests
   distinguish unmatched from handled.
3. **Description that flows back up**: expose the registered
   `(EventKind, Option<Action>)` set and the always/fallback presence from the
   built dispatcher. Generate the GitHub webhook `events` list, or skip the
   `action` probe when no route needs it.
4. **Not** the `DependencyMap`. Panics on missing types, `Any` + `Arc` clone
   per argument, needs `type_check` discipline. "Struct fields are the
   dependencies" already gives compile-time checking for free.

---

## 2. teloxide (dispatcher on dptree)

Source: <https://docs.rs/teloxide/0.17.0/teloxide/dispatching/index.html>.

```rust
pub type UpdateHandler<Err> = dptree::Handler<'static, Result<(), Err>, DpHandlerDescription>;
```

### Filters and composition

```rust
pub trait UpdateFilterExt<Out>: Sealed {
    fn filter_message() -> Handler<'static, Out, DpHandlerDescription>;
    fn filter_edited_message() -> Handler<'static, Out, DpHandlerDescription>;
    // ... 23 methods, one per UpdateKind variant ...
    fn filter_callback_query() -> Handler<'static, Out, DpHandlerDescription>;
}
impl<Out> UpdateFilterExt<Out> for Update where Out: Send + Sync + 'static
```

The update-type filter is spelled `Update::filter_message()` — an associated
function on the *update type*, generated per enum variant, returning a
`filter_map` (Update → Message injected into deps) carrying an `InterestSet`
description of `UpdateKind::Message`.

```rust
pub trait HandlerExt<Output> {
    fn filter_command<C>(self) -> Self where C: BotCommands + Send + Sync + 'static;
    fn filter_mention_command<C>(self) -> Self where C: BotCommands + Send + Sync + 'static;
    fn enter_dialogue<Upd, S, D>(self) -> Self where ...;
}
```

Canonical composition (module docs):

```rust
let command_handler = teloxide::filter_command::<Command, _>()
    .branch(case![State::Start]
        .branch(case![Command::Help].endpoint(help))
        .branch(case![Command::Start].endpoint(start)))
    .branch(case![Command::Cancel].endpoint(cancel));

let message_handler = Update::filter_message()
    .branch(command_handler)
    .branch(case![State::ReceiveFullName].endpoint(receive_full_name))
    .branch(dptree::endpoint(invalid_state));
```

The `middlewares.rs` example shows "before/after" without a middleware
concept:
`.inspect(before).map_async(my_endpoint).inspect(after).endpoint(|result: HandlerResult| async move { result })`.

### Errors, unhandled updates

```rust
pub trait ErrorHandler<E> {
    fn handle_error(self: Arc<Self>, error: E) -> BoxFuture<'static, ()>;
}
// Provided: LoggingErrorHandler, IgnoringErrorHandler, IgnoringErrorHandlerSafe (for Infallible)

pub fn error_handler(self, handler: Arc<dyn ErrorHandler<Err> + Send + Sync>) -> Self   // default LoggingErrorHandler
pub fn default_handler<H, Fut>(self, handler: H) -> Self
where H: Fn(Arc<Update>) -> Fut + Send + Sync + 'static, Fut: Future<Output = ()> + Send + 'static
// "Specifies a handler that will be called for an unhandled update. By default, it is a mere log::warn."
```

Treatment of an update nobody handles (dispatcher.rs 669–689):

```rust
match handler.dispatch(deps).await {
    ControlFlow::Break(Ok(())) => {}
    ControlFlow::Break(Err(err)) => error_handler.clone().handle_error(err).await,
    ControlFlow::Continue(deps) => {
        let update = deps.get();
        (default_handler)(update).await;
    }
}
```

**Unhandled is not an error**; it's a third outcome routed to a separate,
infallible hook whose default is a warning log. The error handler receives
only `E` — no delivery context — which poise fixes (§6). `build()` panics if
`type_check` fails.

### Portable ideas

1. **Separate "unhandled" hook from "error" hook.** Keep `fallback` as the
   routed chain, but return `Unhandled` from `dispatch` when nothing matched
   and let the receiver decide. Strict mode = receiver treats `Unhandled` as
   failure.
2. **Per-kind filter constructors generated from one table** so a kind can't
   be forgotten.
3. **Startup-time validation** instead of first-delivery failure. Prefer
   returning `Result` from `build()` over panicking, since this is a library.

---

## 3. axum

Source: <https://docs.rs/axum/0.8.9/axum/handler/index.html>,
<https://docs.rs/axum/0.8.9/axum/extract/index.html>.

### `Handler<T, S>` and the extractor-tuple trick

```rust
#[diagnostic::on_unimplemented(
    note = "Consider using `#[axum::debug_handler]` to improve the error message"
)]
pub trait Handler<T, S>: Clone + Send + Sync + Sized + 'static {
    type Future: Future<Output = Response> + Send + 'static;
    fn call(self, req: Request, state: S) -> Self::Future;
    fn layer<L>(self, layer: L) -> Layered<L, Self, T, S> where ... { ... }
    fn with_state(self, state: S) -> HandlerService<Self, T, S> { ... }
}
```

```rust
pub trait FromRequestParts<S>: Sized {
    type Rejection: IntoResponse;
    fn from_request_parts(parts: &mut Parts, state: &S)
        -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}
pub trait FromRequest<S, M = ViaRequest>: Sized {
    type Rejection: IntoResponse;
    fn from_request(req: Request<Body>, state: &S)
        -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}
impl<S, T> FromRequest<S, ViaParts> for T where S: Send + Sync, T: FromRequestParts<S>
```

The blanket impl (`axum/handler/mod.rs` 207–262):

```rust
macro_rules! impl_handler {
    ( [$($ty:ident),*], $last:ident ) => {
        #[diagnostic::do_not_recommend]
        #[allow(non_snake_case, unused_mut)]
        impl<F, Fut, S, Res, M, $($ty,)* $last> Handler<(M, $($ty,)* $last,), S> for F
        where
            F: FnOnce($($ty,)* $last,) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = Res> + Send,
            S: Send + Sync + 'static,
            Res: IntoResponse,
            $( $ty: FromRequestParts<S> + Send, )*
            $last: FromRequest<S, M> + Send,
        {
            type Future = Pin<Box<dyn Future<Output = Response> + Send>>;
            fn call(self, req: Request, state: S) -> Self::Future {
                let (mut parts, body) = req.into_parts();
                Box::pin(async move {
                    $(
                        let $ty = match $ty::from_request_parts(&mut parts, &state).await {
                            Ok(value) => value,
                            Err(rejection) => return rejection.into_response(),
                        };
                    )*
                    let req = Request::from_parts(parts, body);
                    let $last = match $last::from_request(req, &state).await {
                        Ok(value) => value,
                        Err(rejection) => return rejection.into_response(),
                    };
                    self($($ty,)* $last,).await.into_response()
                })
            }
        }
    };
}
all_the_tuples!(impl_handler);   // arities 1..=16
```

How the "function with N extractor args implements `Handler`" works:

1. One blanket impl per arity, generated by macro for 0..=16 arguments.
2. The `T` parameter is a coherence discriminator. Docs: "The type parameter
   `T` is a workaround for trait coherence rules, allowing us to write blanket
   implementations of `Handler` over many types of handler functions with
   different numbers of arguments, without the compiler forbidding us from
   doing so because one type `F` can in theory implement both `Fn(A) -> X`
   and `Fn(A, B) -> Y`."
3. The `M` marker on `FromRequest<S, M>` (`ViaRequest` vs `ViaParts`) lets the
   *last* argument be either kind without the two impls overlapping.
   Documented consequence: "Cannot implement both `FromRequest` and
   `FromRequestParts`" for one type.
4. Ordering rule enforced by types: all but the last must be
   `FromRequestParts`, the last `FromRequest`.
5. `F: Clone` because `call(self, ...)` consumes the handler per request.
6. Returned future is always `Pin<Box<dyn Future + Send>>`.

### Diagnostics story

- `#[diagnostic::on_unimplemented(note = ...)]` on the trait and
  `#[diagnostic::do_not_recommend]` on every blanket impl (stable since Rust
  1.78 / 1.85), so rustc doesn't dump the 17 candidate impls.
- The docs are honest: "Unfortunately Rust gives poor error messages if you
  try to use a function that doesn't quite match what's required by
  `Handler`."
- `#[debug_handler]` (axum-macros) rewrites the function into per-argument
  check functions so the error lands on the offending argument. "This macro
  has no effect when compiled with the release profile."

### `State<S>`, `FromRef`, `Extension<T>`

```rust
pub struct State<S>(pub S);
impl<OuterState, InnerState> FromRequestParts<OuterState> for State<InnerState>
where InnerState: FromRef<OuterState>, OuterState: Send + Sync
{ type Rejection = Infallible; ... }
```

"extracting a state of the wrong type results in a compile error";
`Router<S>` "means a router that is *missing* a state of type `S`".
`Extension<T>` fails at runtime if absent — the docs steer library authors to
`State` + `FromRef<S>` instead.

### `Router::fallback`, `layer` vs `route_layer`

```rust
pub fn fallback<H, T>(self, handler: H) -> Self where H: Handler<T, S>, T: 'static
```

"This service will be called if no routes matches the incoming request. … If a
handler is matched by a request but returns 404 the fallback is not called."
Distinct `method_not_allowed_fallback` for "a route exists, but the method of
the request is not supported." `merge` panics "If two routers that each have
a fallback are merged"; `reset_fallback()` resolves that.

```rust
pub fn layer<L>(self, layer: L) -> Router<S>        // all routes incl. fallback
pub fn route_layer<L>(self, layer: L) -> Self       // "will only run if the request matches a route"
```

"This is useful for middleware that return early (such as authorization)
which might otherwise convert a `404 Not Found` into a `401 Unauthorized`.
This function will panic if no routes have been declared yet on the router."

### Portable ideas

1. **`#[diagnostic::on_unimplemented]` + `#[diagnostic::do_not_recommend]`**
   on the handler traits and blanket impls. Zero cost.
2. **The 404/405 split → kind-unknown vs action-unknown.** The two-level map
   already knows whether the kind matched but the action didn't. Expose that
   as a distinct miss.
3. **`route_layer` semantics = "only when matched."** `always` is axum's
   `layer`; a "matched-only" pre-hook is `route_layer`.
4. **Do not port the extractor-tuple machinery.** 17 blanket impls, two
   marker type parameters, and a proc-macro to make errors readable. An
   explicit `into_webhook_handler()` conversion is the cheap alternative.

---

## 4. tower

Source: <https://docs.rs/tower/0.5.3/tower/>,
<https://docs.rs/tower/0.5.3/tower/steer/index.html>.

```rust
pub trait Service<Request> {
    type Response;
    type Error;
    type Future: Future<Output = Result<Self::Response, Self::Error>>;
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;
    fn call(&mut self, req: Request) -> Self::Future;
}
pub trait Layer<S> {
    type Service;
    fn layer(&self, inner: S) -> Self::Service;
}
```

Contract details: "Implementations are permitted to panic if `call` is invoked
without obtaining `Poll::Ready(Ok(()))` from `poll_ready`"; "Be careful when
cloning inner services… the clone might not be [ready]".

```rust
// ServiceExt (feature "util")
fn map_err<F, Error>(self, f: F) -> MapErr<Self, F> where F: FnOnce(Self::Error) -> Error + Clone;
fn filter<F, NewRequest>(self, filter: F) -> Filter<Self, F> where F: Predicate<NewRequest>;   // rejects with error, not "skip"
fn boxed(self) -> BoxService<Request, Self::Response, Self::Error> where Self: Send + 'static, Self::Future: Send + 'static;
```

```rust
// Timeout (feature "timeout", depends on tokio)
pub struct Timeout<T> { /* inner, Duration */ }
impl<S, Request> Service<Request> for Timeout<S>
where S: Service<Request>, S::Error: Into<BoxError>
{ type Response = S::Response; type Error = Box<dyn Error + Send + Sync>; ... }
```

### `Steer`

```rust
pub struct Steer<S, F, Req> { /* private */ }
impl<S, F, Req> Steer<S, F, Req> {
    pub fn new(services: impl IntoIterator<Item = S>, router: F) -> Self
}
pub trait Picker<S, Req> {
    fn pick(&mut self, r: &Req, services: &[S]) -> usize;
}
```

From the struct docs: it "1. Determines, via the provided `Picker`, which
`Service` the request corresponds to. 2. Waits (in `Service::poll_ready`) for
*all* services to be ready. 3. Calls the correct `Service`." And: "`Steer`
must wait for all services to be ready since it can't know ahead of time
which `Service` the next message will arrive for … This will cause
head-of-line blocking unless paired with a `Service` that does buffer items
indefinitely."

Other limits: all targets share one type `S`, the picker is synchronous and
returns a bare `usize` (no notion of "no match"), an out-of-range index is a
slice-indexing panic. No fallback tier, no always tier, no chaining.

### Portable ideas

1. **Decorator structs instead of a `Layer` trait.** `Timeout<S>` is just a
   struct holding `S` that re-implements the trait. Take timers as
   caller-supplied futures to stay sans-I/O and wasm-clean (tower's own
   `Timeout` hard-depends on tokio).
2. **`map_err`-style adapter at registration** rather than requiring `From`
   on the dispatcher error.
3. **Do not adopt `tower::Service` for handlers.** `poll_ready` buys nothing
   for a request-at-a-time sans-I/O receiver and adds a panic-on-misuse
   contract. `Steer` is strictly weaker than a keyed map.

---

## 5. serenity `EventHandler` vs twilight `Event`

Sources: <https://docs.rs/serenity/0.12.5/serenity/client/trait.EventHandler.html>,
<https://docs.rs/twilight-gateway/0.17.1/>,
<https://docs.rs/twilight-model/0.17.1/twilight_model/gateway/event/enum.Event.html>.

### serenity: one method per event, all defaulted

```rust
#[async_trait]
pub trait EventHandler: Send + Sync {
    // 79 provided methods, e.g.
    async fn message(&self, ctx: Context, new_message: Message) { drop((ctx, new_message)) }
    async fn message_delete(&self, ctx: Context, channel_id: ChannelId, deleted_message_id: MessageId, guild_id: Option<GuildId>) { ... }
}

#[async_trait]
pub trait RawEventHandler: Send + Sync {
    async fn raw_event(&self, _ctx: Context, _ev: Event) {}
}
```

Generated from one table by `event_handler!` (event_handler.rs 8–81), which
emits **three artefacts from one source**: the trait (every method a no-op
default), a `FullEvent` enum (`#[non_exhaustive]`,
`#[allow(clippy::large_enum_variant)] // TODO: do some boxing to fix this`),
`FullEvent::snake_case_name()`, and

```rust
pub async fn dispatch(self, ctx: Context, handler: &dyn EventHandler) {
    match self { Self::$variant_name { $( $arg_name ),* } => handler.$method_name(ctx, $( $arg_name ),*).await, ... }
}
```

Trade-offs: every call boxes a `Send` future (`#[async_trait]`), even the
no-ops; return type `()` so no error channel; one handler object per client;
discoverable and non-breaking to extend, but no exhaustiveness check.

### twilight: no handler trait, match on an enum

```rust
pub enum Event {
    // 74 variants; large payloads boxed:
    MessageCreate(Box<MessageCreate>), MessageDelete(MessageDelete), ...
}
impl Event {
    pub const fn guild_id(&self) -> Option<Id<GuildMarker>>;
    pub const fn kind(&self) -> EventType;
}
pub struct EventTypeFlags(/* u128 bitflags */);
pub fn parse(json, wanted_event_types) // "Parse a JSON encoded gateway event into a GatewayEvent if wanted_event_types contains its type."
```

`EventTypeFlags` docs: "event type flags are a Twilight-specific technique to
filter out individual events from being deserialized at all, effectively
discarding them." Uninteresting events never get past the opcode/`t` header.

Trade-offs: plain data + `match` composes with ordinary functions and supports
exhaustive matching (twilight's `Event` is *not* `#[non_exhaustive]`; a new
variant is a breaking change by design). Costs: large enum → boxing decisions
per variant; every consumer writes the `match`.

### Portable ideas

1. **One table → trait + enum + dispatch** (serenity's macro). A single table
   of `(wire name, EventKind variant, payload type)` generating `Payload`
   impls, the `EventKind` parser, and per-kind helpers, so kind ↔ payload ↔
   header can't drift.
2. **Decide "kind" before decoding** (twilight). Probe `action` only when a
   registered route depends on it.
3. **Prefer enum+match / small traits over a wide default-method trait.**

---

## 6. poise

Source: <https://docs.rs/poise/0.6.2/poise/>.

```rust
pub struct FrameworkOptions<U, E> {
    pub commands: Vec<Command<U, E>>,
    pub on_error: fn(FrameworkError<'_, U, E>) -> BoxFuture<'_, ()>,
    pub pre_command: fn(Context<'_, U, E>) -> BoxFuture<'_, ()>,
    pub post_command: fn(Context<'_, U, E>) -> BoxFuture<'_, ()>,
    pub command_check: Option<fn(Context<'_, U, E>) -> BoxFuture<'_, Result<bool, E>>>,
    pub event_handler: for<'a> fn(&'a Context, &'a FullEvent, FrameworkContext<'a, U, E>, &'a U) -> BoxFuture<'a, Result<(), E>>,
    ...
}
```

All hooks are plain `fn` pointers; state travels through `U`. `event_handler`
receives `&FullEvent` and returns `Result<(), E>`; the framework converts an
`Err` into:

```rust
pub enum FrameworkError<'a, U, E> {
    #[non_exhaustive] EventHandler { error: E, ctx: &'a Context, event: &'a FullEvent, framework: FrameworkContext<'a, U, E> },
    #[non_exhaustive] Command { error: E, ctx: Context<'a, U, E> },
    UnknownCommand { .. }, UnknownInteraction { .. }, CommandPanic { payload: Option<String>, .. }, ...
}
```

### Portable ideas

1. **Error carries the position it happened at.** `DispatchError<E> { source: E,
   delivery_id, kind, action, tier, handler: &'static Location }`. The
   delivery ID is the redelivery key, and "which handler at which tier" is
   what an operator greps for.
2. **"Unknown" as a named, non-error variant.**
3. **`fn` pointers vs closures for hooks**: conflicts with "handler is a
   struct whose fields are its dependencies"; possibly fine for a purely
   observational hook.

---

## 7. "Handler decides what happens next" vs `Result<(), E>`

```rust
// std::ops::ControlFlow (stable since 1.55)
pub enum ControlFlow<B, C = ()> { Continue(C), Break(B) }
```

| Library | Handler returns | "Not mine" signal | "Failed" signal | Who decides matching |
|---|---|---|---|---|
| dptree/teloxide | `ControlFlow<Result<(), E>, DependencyMap>` | `Continue(deps)` | `Break(Err(e))` | the handler (filters are handlers) |
| axum | `Response` | none at handler level; `Router::fallback` on route miss | a `Response` | the router, statically |
| tower `Steer` | `Result<Resp, Err>` | none; picker returns an index | `Err` | the picker, synchronously |
| serenity | `()` | implicit (no-op default) | none | the dispatch `match`, statically |
| poise `event_handler` | `Result<(), E>` | none (always called) | `Err` → `FrameworkError::EventHandler` | n/a |
| octoevents (0.1) | `Result<(), E>` | none at handler level; fallback on route miss | `Err(E)` via `From` | the `EventMatcher`, statically |

The dptree shape is the only one where matching is *dynamic and composable*.
The price is that every hop must thread the input back through `Continue`.
The axum/octoevents shape keeps matching *static*, which makes "strict
fallback rejects kinds nothing handles" a property of the route table rather
than of runtime behaviour — analysable at build time and introspectable.

A middle ground: let the *dispatcher* return an `Outcome` (matched-and-ok,
matched-and-failed, unmatched-and-fallback-ran, unmatched-and-no-fallback)
while handlers keep returning `Result<(), E>`.

---

## Synthesis: transferable ideas ranked by value ÷ cost

Assumes: small, typed, sans-I/O, `MaybeSend`, must compile on wasm32.

1. **Dispatch outcome distinguishes unmatched from handled** (teloxide, poise).
2. **Contextual dispatch error** (poise `FrameworkError::EventHandler`).
3. **`#[track_caller]` + `Location::caller()` at every registration** (dptree).
4. **`#[diagnostic::on_unimplemented]` / `#[diagnostic::do_not_recommend]`** (axum).
5. **Build-time route-table validation returning `Result`** (dptree, axum).
6. **Kind-unknown vs action-unknown miss** (axum fallback vs `method_not_allowed_fallback`).
7. **Introspection of the route table** (dptree `InterestSet`, teloxide `allowed_updates`).
8. **Decorator handlers as the middleware story** (tower `Timeout<S>`, `map_err`).
9. **Single blanket impl per handler flavour** (the useful 10% of axum's extractor trick).
10. **One table generating kind ↔ payload ↔ wire name** (serenity `event_handler!`).

Explicitly rejected after reading the sources:

- **Runtime DI container** (dptree `DependencyMap`).
- **Extractor-tuple `Handler<T, S>`** (axum).
- **`tower::Service`/`Steer` for handlers**.
- **Wide default-method trait** (serenity).
