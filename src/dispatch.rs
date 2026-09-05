use std::{collections::HashMap, fmt, future::Future, sync::Arc};

#[cfg(feature = "octocrab")]
use octocrab::models::webhook_events::WebhookEvent;

use crate::{
    Action, DecodeError, Envelope, EventKind, EventMeta, MaybeSend, MaybeSync, MetaHandler,
    Payload, PayloadHandler, WebhookHandler, matcher::Slot, runtime::BoxFuture, trace,
};
#[cfg(feature = "octocrab")]
use crate::{EventHandler, EventMatcher};

// Erased handlers. A trait object admits only one non-auto trait, so these
// cannot be written as `dyn Fn(..) + MaybeSend + MaybeSync` and carry the
// platform split by hand; see `runtime` for the rationale.
#[cfg(not(target_arch = "wasm32"))]
type MetaFn<E> = Arc<dyn Fn(EventMeta) -> BoxFuture<Result<(), E>> + Send + Sync + 'static>;
#[cfg(target_arch = "wasm32")]
type MetaFn<E> = Arc<dyn Fn(EventMeta) -> BoxFuture<Result<(), E>> + 'static>;

#[cfg(all(feature = "octocrab", not(target_arch = "wasm32")))]
type EventFn<E> =
    Arc<dyn Fn(EventMeta, WebhookEvent) -> BoxFuture<Result<(), E>> + Send + Sync + 'static>;
#[cfg(all(feature = "octocrab", target_arch = "wasm32"))]
type EventFn<E> = Arc<dyn Fn(EventMeta, WebhookEvent) -> BoxFuture<Result<(), E>> + 'static>;

#[cfg(not(target_arch = "wasm32"))]
type EnvelopeFn<E> = Arc<dyn Fn(Envelope) -> BoxFuture<Result<(), E>> + Send + Sync + 'static>;
#[cfg(target_arch = "wasm32")]
type EnvelopeFn<E> = Arc<dyn Fn(Envelope) -> BoxFuture<Result<(), E>> + 'static>;

/// A handler that routes verified envelopes to other handlers by kind and
/// action.
///
/// Each tier accepts one handler flavour, chosen by what the tier can promise
/// to have: `always_raw` takes a [`WebhookHandler`], `always` and `fallback`
/// take a [`MetaHandler`], `on_payload` and `on_payload_action` take a
/// [`PayloadHandler`] whose kind comes from its payload type, and `on` takes
/// an `EventHandler` for the kinds and actions a matcher selects. Each
/// handler keeps its own error type; the dispatcher converts them into `E`
/// through `From` at registration.
///
/// Per delivery the dispatcher runs the raw chain, then the `always` chain,
/// then the chain for the envelope's kind and action, then the kind-wide
/// chain, and the `fallback` chain only if neither routed chain matched.
/// Every chain is sequential, in registration order, and stops at the first
/// error. The raw and `always` chains never count as a match, and an empty
/// fallback chain succeeds, so unmatched kinds are green in GitHub until you
/// decide otherwise.
///
/// The decode rule: raw, meta and payload handlers never decode with
/// octocrab; the first event handler reached does, once, and every later one
/// shares the result. The raw tier receives the bytes as they were verified,
/// the meta tiers decode nothing, and payload handlers decode their own type
/// from the raw bytes, so a payload octocrab cannot represent still reaches
/// `always_raw`, `always` and every payload handler, a strict `fallback`
/// answers it with its own error rather than a decode error, and the delivery
/// fails only at the first event handler. A routed handler decodes only when
/// its route matches: a payload handler registered for some actions decodes
/// nothing for a delivery carrying another.
///
/// ```
/// use octoevents::{Action, DecodeError, Dispatcher, Envelope, EventKind, EventMeta};
///
/// #[derive(Debug)]
/// enum AppError { Decode(DecodeError), Unhandled(EventKind) }
/// impl From<DecodeError> for AppError {
///     fn from(error: DecodeError) -> Self { Self::Decode(error) }
/// }
/// impl From<std::convert::Infallible> for AppError {
///     fn from(never: std::convert::Infallible) -> Self { match never {} }
/// }
///
/// // A consumer view over the pull-request payload; the kind it declares is
/// // the kind its handler is routed by.
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .always_raw(|envelope: Envelope| async move {
///         println!("store {} ({} bytes)", envelope.meta.delivery_id, envelope.raw.len());
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .always(|meta: EventMeta| async move {
///         println!("{} {} {:?}", meta.delivery_id, meta.kind, meta.action);
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .on_payload(|meta: EventMeta, pr: PullRequestNumber| async move {
///         println!("PR #{} {:?} for installation {:?}", pr.number, meta.action, meta.installation_id);
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .on_payload_action([Action::Opened, Action::Reopened], |_: EventMeta, pr: PullRequestNumber| async move {
///         println!("label PR #{}", pr.number);
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .fallback(|meta: EventMeta| async move {
///         Err::<(), _>(AppError::Unhandled(meta.kind))
///     })
///     .build();
/// # let _ = dispatcher;
/// ```
///
/// `E` must implement `From` of every registered handler's error, including
/// [`Infallible`](std::convert::Infallible) for handlers that cannot fail;
/// that impl is the one-line `match never {}` above. Handlers written against
/// `E` itself need nothing further.
///
/// Everything above is part of the crate's core. `on` is the one method that
/// needs the `octocrab` feature: it routes an event handler over octocrab's
/// decoded `WebhookEvent`, for logic that spans kinds.
///
/// ```
/// # #[cfg(feature = "octocrab")] {
/// use octocrab::models::webhook_events::WebhookEvent;
/// use octoevents::{Action, Dispatcher, EventKind, EventMeta};
/// # use octoevents::DecodeError;
/// # struct AppError;
/// # impl From<DecodeError> for AppError { fn from(_: DecodeError) -> Self { Self } }
/// # impl From<std::convert::Infallible> for AppError {
/// #     fn from(never: std::convert::Infallible) -> Self { match never {} }
/// # }
///
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .on((EventKind::PullRequest, [Action::Opened, Action::Synchronize]), |meta: EventMeta, event: WebhookEvent| async move {
///         println!("triage {:?} for {:?}", meta.action, event.repository.map(|repository| repository.name));
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .build();
/// # let _ = dispatcher;
/// # }
/// ```
///
/// The raw tier is the one place a [`WebhookHandler`] enters the dispatcher:
/// work on the bytes (persist, forward) is registered with `always_raw` and
/// runs before any typed handler. It can continue or fail, never skip: a
/// webhook handler that decides whether to route at all wraps the dispatcher
/// instead. There is no raw fallback: a strict `fallback` reports an
/// unmatched delivery through its error, and a wrapping handler still holds
/// the bytes when it does.
///
/// Enabling the `octocrab` feature makes octocrab's pre-1.0 version part of
/// this crate's public API: an octocrab major bump is a breaking change for
/// `on` and the `EventHandler` it accepts, not for the rest of this type.
pub struct Dispatcher<E> {
    routes: Arc<Routes<E>>,
}

impl<E> Clone for Dispatcher<E> {
    fn clone(&self) -> Self {
        Self {
            routes: Arc::clone(&self.routes),
        }
    }
}

impl<E> fmt::Debug for Dispatcher<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.routes.fmt_as("Dispatcher", formatter)
    }
}

impl<E> Dispatcher<E>
where
    E: From<DecodeError> + 'static,
{
    /// Starts building a dispatcher whose unmatched deliveries succeed.
    #[must_use]
    pub fn builder() -> DispatcherBuilder<E> {
        DispatcherBuilder::default()
    }

    /// Runs the raw chain, the `always` chain, the matching routed chains, and
    /// the fallback chain when nothing matched, in that order.
    ///
    /// # Errors
    ///
    /// Stops and returns the first handler error, or the decode error of the
    /// first handler whose input could not be decoded.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            name = "octoevents.dispatch",
            skip_all,
            fields(delivery_id = %envelope.meta.delivery_id, event = %envelope.meta.kind, outcome = tracing::field::Empty)
        )
    )]
    pub async fn dispatch(&self, envelope: Envelope) -> Result<(), E> {
        let mut in_flight = InFlight {
            envelope: &envelope,
            #[cfg(feature = "octocrab")]
            event: None,
        };

        in_flight
            .run_chain(&self.routes.raw)
            .await
            .inspect_err(|_| trace::record("outcome", "handler_error"))?;

        in_flight
            .run_chain(&self.routes.always)
            .await
            .inspect_err(|_| trace::record("outcome", "handler_error"))?;

        // Routes are keyed by kind first so a delivery is looked up entirely by
        // reference: no EventKind or Action is cloned to build a lookup key.
        let routes = self.routes.by_kind.get(&envelope.meta.kind);
        let specific = routes.and_then(|routes| {
            envelope
                .meta
                .action
                .as_ref()
                .and_then(|action| routes.by_action.get(action))
        });
        let any_action = routes
            .map(|routes| &routes.any_action)
            .filter(|chain| !chain.is_empty());

        let mut matched = false;
        for chain in specific.into_iter().chain(any_action) {
            matched = true;
            in_flight
                .run_chain(chain)
                .await
                .inspect_err(|_| trace::record("outcome", "handler_error"))?;
        }

        if matched {
            trace::record("outcome", "ok");
            return Ok(());
        }

        let result = in_flight.run_chain(&self.routes.fallback).await;
        trace::record(
            "outcome",
            if result.is_ok() {
                "fallback_ok"
            } else {
                "fallback_error"
            },
        );
        result
    }
}

impl<E> WebhookHandler for Dispatcher<E>
where
    E: From<DecodeError> + 'static,
{
    type Error = E;

    fn handle(&self, envelope: Envelope) -> impl Future<Output = Result<(), E>> + MaybeSend {
        self.dispatch(envelope)
    }
}

/// One envelope being dispatched: the envelope plus, with the `octocrab`
/// feature, the lazily decoded event that every event route shares.
struct InFlight<'a> {
    envelope: &'a Envelope,
    #[cfg(feature = "octocrab")]
    event: Option<WebhookEvent>,
}

impl InFlight<'_> {
    /// Runs one chain in order, stopping at the first error.
    async fn run_chain<E>(&mut self, chain: &[Route<E>]) -> Result<(), E>
    where
        E: From<DecodeError>,
    {
        for route in chain {
            self.run(route).await?;
        }
        Ok(())
    }

    async fn run<E>(&mut self, route: &Route<E>) -> Result<(), E>
    where
        E: From<DecodeError>,
    {
        match route {
            Route::Raw(handler) | Route::Payload(handler) => handler(self.envelope.clone()).await,
            Route::Meta(handler) => handler(self.envelope.meta.clone()).await,
            #[cfg(feature = "octocrab")]
            Route::Event(handler) => {
                // Decoded on first use and cloned per event route: a clone is
                // far cheaper than decoding a payload that can run to megabytes.
                let event = if let Some(event) = &self.event {
                    event.clone()
                } else {
                    let event = self.envelope.decode_event().map_err(E::from)?;
                    self.event.insert(event).clone()
                };
                handler(self.envelope.meta.clone(), event).await
            }
        }
    }
}

/// A builder for [`Dispatcher`].
pub struct DispatcherBuilder<E> {
    routes: Routes<E>,
}

impl<E> fmt::Debug for DispatcherBuilder<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.routes.fmt_as("DispatcherBuilder", formatter)
    }
}

impl<E> Default for DispatcherBuilder<E> {
    fn default() -> Self {
        Self {
            routes: Routes {
                raw: Vec::new(),
                always: Vec::new(),
                by_kind: HashMap::new(),
                fallback: Vec::new(),
            },
        }
    }
}

impl<E> DispatcherBuilder<E>
where
    E: From<DecodeError> + 'static,
{
    /// Registers a webhook handler that runs for every delivery, before every
    /// other tier.
    ///
    /// The raw tier receives the verified [`Envelope`], bytes included: the
    /// place to persist or forward the envelope before anything is routed.
    /// Its failure fails the delivery, and it never counts as a match, so a
    /// strict fallback still rejects kinds nothing else handles. Nothing is
    /// decoded on its behalf.
    #[must_use]
    pub fn always_raw<H>(mut self, handler: H) -> Self
    where
        H: WebhookHandler + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.routes.raw.push(raw_route(handler));
        self
    }

    /// Registers a meta handler that runs for every delivery, after the raw
    /// tier and before routing.
    ///
    /// The place for audit, metrics, and deduplication: its failure fails the
    /// delivery, and it never counts as a match, so a strict fallback still
    /// rejects kinds nothing else handles. It receives only the [`EventMeta`],
    /// so it runs even for a payload no typed handler can decode.
    #[must_use]
    pub fn always<H>(mut self, handler: H) -> Self
    where
        H: MetaHandler + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.routes.always.push(meta_route(handler));
        self
    }

    /// Registers an event handler for the kinds and actions the matcher
    /// selects.
    ///
    /// A handler registered under several slots is shared, not duplicated.
    /// The first event handler a delivery reaches decodes octocrab's
    /// `WebhookEvent` once, and every later one shares it; a payload octocrab
    /// cannot represent fails the delivery at that position.
    ///
    /// This is the one registration that needs the `octocrab` feature.
    #[cfg(feature = "octocrab")]
    #[must_use]
    pub fn on<H>(mut self, matcher: impl Into<EventMatcher>, handler: H) -> Self
    where
        H: EventHandler + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.insert_each(matcher.into().into_slots(), &event_route(handler));
        self
    }

    /// Registers a payload handler for every action of the kind its payload
    /// type declares.
    ///
    /// No matcher is needed, and none is accepted: the kind is `P::KIND`, so a
    /// pull-request handler cannot end up under `issues`. To run for some
    /// actions only, register with [`on_payload_action`](Self::on_payload_action)
    /// rather than filtering inside the handler: a route that does not match
    /// neither counts as a match nor decodes the payload. A handler that
    /// needs the action reads `meta.action`, the crate's [`Action`], whose
    /// [`Unknown`](Action::Unknown) carries a value this crate does not know;
    /// octocrab's per-kind action enums have no such catch-all, so a payload
    /// type built on them cannot decode an action they do not know.
    ///
    /// `P` is inferred from a closure's parameter type or from a struct that
    /// implements [`PayloadHandler`] for one payload. A struct that
    /// implements it for several needs the payload named:
    /// `on_payload::<PullRequestNumber, _>(labeler)`.
    #[must_use]
    pub fn on_payload<P, H>(mut self, handler: H) -> Self
    where
        P: Payload + 'static,
        H: PayloadHandler<P> + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.insert(Slot::any_action(P::KIND), payload_route(handler));
        self
    }

    /// Registers a payload handler for the given actions of the kind its
    /// payload type declares.
    ///
    /// The route is `P::KIND` with each action in turn, so this mirrors
    /// `on((kind, [actions]), handler)` with the kind supplied by the type.
    /// A delivery whose action is not listed does not match: a strict
    /// fallback rejects `pull_request.closed` when only `opened` is
    /// registered, and the payload is not decoded for it, so a payload type
    /// that cannot represent an action (octocrab's per-kind action enums have
    /// no catch-all) fails only the deliveries it was registered for. The
    /// handler is shared across the actions, not duplicated, and runs before
    /// any kind-wide [`on_payload`](Self::on_payload) route for the same kind.
    ///
    /// Any collection of [`Action`]s is accepted; an array literal is the
    /// usual shape, and an empty one registers nothing. `P` is inferred as
    /// for `on_payload`, and a struct that implements [`PayloadHandler`] for
    /// several payloads names it the same way:
    /// `on_payload_action::<PullRequestNumber, _>([Action::Opened], labeler)`.
    #[must_use]
    pub fn on_payload_action<P, H>(
        mut self,
        actions: impl IntoIterator<Item = Action>,
        handler: H,
    ) -> Self
    where
        P: Payload + 'static,
        H: PayloadHandler<P> + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        let slots = actions
            .into_iter()
            .map(|action| Slot::action(P::KIND, action));
        self.insert_each(slots, &payload_route(handler));
        self
    }

    /// Appends a meta handler to the chain that runs when no routed chain
    /// matched.
    ///
    /// Several may be registered; they run in order and stop at the first
    /// error. "Log it, then reject it" is two small handlers. Like `always`,
    /// the chain receives only the [`EventMeta`], so a strict fallback reports
    /// its own error for an unmatched payload nothing can decode, not a
    /// decode error.
    #[must_use]
    pub fn fallback<H>(mut self, handler: H) -> Self
    where
        H: MetaHandler + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.routes.fallback.push(meta_route(handler));
        self
    }

    /// Finishes the dispatcher.
    #[must_use]
    pub fn build(self) -> Dispatcher<E> {
        Dispatcher {
            routes: Arc::new(self.routes),
        }
    }

    /// Registers one handler under every slot; the route is shared, not
    /// duplicated.
    fn insert_each(&mut self, slots: impl IntoIterator<Item = Slot>, route: &Route<E>) {
        for slot in slots {
            self.insert(slot, route.clone());
        }
    }

    fn insert(&mut self, slot: Slot, route: Route<E>) {
        let routes = self.routes.by_kind.entry(slot.kind).or_default();
        let chain = match slot.action {
            Some(action) => routes.by_action.entry(action).or_default(),
            None => &mut routes.any_action,
        };
        chain.push(route);
    }
}

fn raw_route<E, H>(handler: H) -> Route<E>
where
    E: From<H::Error> + 'static,
    H: WebhookHandler + MaybeSend + MaybeSync + 'static,
{
    let handler = Arc::new(handler);
    Route::Raw(Arc::new(move |envelope: Envelope| {
        let handler = Arc::clone(&handler);
        Box::pin(async move { handler.handle(envelope).await.map_err(E::from) })
    }))
}

fn meta_route<E, H>(handler: H) -> Route<E>
where
    E: From<H::Error> + 'static,
    H: MetaHandler + MaybeSend + MaybeSync + 'static,
{
    let handler = Arc::new(handler);
    Route::Meta(Arc::new(move |meta: EventMeta| {
        let handler = Arc::clone(&handler);
        Box::pin(async move { handler.handle(meta).await.map_err(E::from) })
    }))
}

#[cfg(feature = "octocrab")]
fn event_route<E, H>(handler: H) -> Route<E>
where
    E: From<H::Error> + 'static,
    H: EventHandler + MaybeSend + MaybeSync + 'static,
{
    let handler = Arc::new(handler);
    Route::Event(Arc::new(move |meta: EventMeta, event: WebhookEvent| {
        let handler = Arc::clone(&handler);
        Box::pin(async move { handler.handle(meta, event).await.map_err(E::from) })
    }))
}

fn payload_route<E, P, H>(handler: H) -> Route<E>
where
    E: From<DecodeError> + From<H::Error> + 'static,
    P: Payload + 'static,
    H: PayloadHandler<P> + MaybeSend + MaybeSync + 'static,
{
    let handler = Arc::new(handler);
    Route::Payload(Arc::new(move |envelope: Envelope| {
        let handler = Arc::clone(&handler);
        Box::pin(async move {
            // Routing already guaranteed the kind, so the unchecked `decode`
            // rather than `decode_payload`: only the shape of the payload can
            // still disagree.
            let payload = envelope.decode::<P>().map_err(E::from)?;
            handler
                .handle(envelope.meta, payload)
                .await
                .map_err(E::from)
        })
    }))
}

/// A registered handler, erased to its error type but keeping its flavour so
/// dispatch knows which input to prepare.
enum Route<E> {
    /// Takes the whole envelope, bytes included; nothing to decode.
    Raw(EnvelopeFn<E>),
    /// Takes the metadata alone; nothing to decode.
    Meta(MetaFn<E>),
    /// Takes the shared decoded event.
    #[cfg(feature = "octocrab")]
    Event(EventFn<E>),
    /// Takes the envelope and decodes its own payload type.
    Payload(EnvelopeFn<E>),
}

impl<E> Clone for Route<E> {
    fn clone(&self) -> Self {
        match self {
            Self::Raw(handler) => Self::Raw(Arc::clone(handler)),
            Self::Meta(handler) => Self::Meta(Arc::clone(handler)),
            #[cfg(feature = "octocrab")]
            Self::Event(handler) => Self::Event(Arc::clone(handler)),
            Self::Payload(handler) => Self::Payload(Arc::clone(handler)),
        }
    }
}

// The erased handler is never `Debug`; its flavour is what dispatch acts on,
// so that is what prints, with the handler elided as `Meta(..)`. No bound on
// `E`: the dispatcher is `Debug` for any error type, as it is `Clone` for any.
impl<E> fmt::Debug for Route<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flavour = match self {
            Self::Raw(_) => "Raw",
            Self::Meta(_) => "Meta",
            #[cfg(feature = "octocrab")]
            Self::Event(_) => "Event",
            Self::Payload(_) => "Payload",
        };
        formatter.debug_tuple(flavour).finish_non_exhaustive()
    }
}

/// Every chain a dispatcher can run.
struct Routes<E> {
    raw: Vec<Route<E>>,
    always: Vec<Route<E>>,
    by_kind: HashMap<EventKind, KindRoutes<E>>,
    fallback: Vec<Route<E>>,
}

impl<E> Routes<E> {
    /// Prints the route table under the name of the type that owns it, so
    /// the dispatcher and its builder read alike.
    fn fmt_as(&self, name: &str, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(name)
            .field("raw", &self.raw)
            .field("always", &self.always)
            .field("by_kind", &self.by_kind)
            .field("fallback", &self.fallback)
            .finish()
    }
}

/// Every handler chain registered for one event kind.
struct KindRoutes<E> {
    any_action: Vec<Route<E>>,
    by_action: HashMap<Action, Vec<Route<E>>>,
}

impl<E> Default for KindRoutes<E> {
    fn default() -> Self {
        Self {
            any_action: Vec::new(),
            by_action: HashMap::new(),
        }
    }
}

impl<E> fmt::Debug for KindRoutes<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KindRoutes")
            .field("any_action", &self.any_action)
            .field("by_action", &self.by_action)
            .finish()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::{future::Future, pin::Pin, sync::Arc};

    #[cfg(feature = "octocrab")]
    use octocrab::models::webhook_events::WebhookEvent;
    use tokio::sync::Mutex;

    use super::Dispatcher;
    #[cfg(feature = "octocrab")]
    use crate::EventMatcher;
    use crate::{
        Action, DecodeError, Envelope, EventKind, EventMeta,
        test_support::{
            AppError, check_run_completed, envelope_with_action, installation_created, ping,
            pull_request, pull_request_opened, unknown, unrepresentable,
        },
    };

    type Calls = Arc<Mutex<Vec<&'static str>>>;
    type Recorded<E> = Pin<Box<dyn Future<Output = Result<(), E>> + Send>>;

    /// A consumer view that accepts any `pull_request` payload, so a payload
    /// route for that kind can be registered with or without octocrab.
    #[derive(serde::Deserialize)]
    struct AnyPullRequest {}
    crate::impl_payload!(AnyPullRequest => EventKind::PullRequest);

    /// A meta handler that appends `value` to the shared log.
    fn record(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(EventMeta) -> Recorded<AppError> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Ok(())
            })
        }
    }

    /// A meta handler that appends `value` to the shared log and then fails.
    fn fail(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(EventMeta) -> Recorded<&'static str> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Err(value)
            })
        }
    }

    /// A webhook handler that appends `value` to the shared log.
    fn record_envelope(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(Envelope) -> Recorded<AppError> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Ok(())
            })
        }
    }

    /// A webhook handler that appends `value` to the shared log and then fails.
    fn fail_envelope(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(Envelope) -> Recorded<&'static str> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Err(value)
            })
        }
    }

    /// A payload handler over [`AnyPullRequest`] that appends `value` to the
    /// shared log.
    fn record_payload(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(EventMeta, AnyPullRequest) -> Recorded<AppError> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_, _| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Ok(())
            })
        }
    }

    /// An event handler that appends `value` to the shared log.
    #[cfg(feature = "octocrab")]
    fn record_event(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(EventMeta, WebhookEvent) -> Recorded<AppError> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_, _| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Ok(())
            })
        }
    }

    /// An event handler that appends `value` to the shared log and then fails.
    #[cfg(feature = "octocrab")]
    fn fail_event(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(EventMeta, WebhookEvent) -> Recorded<&'static str> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_, _| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Err(value)
            })
        }
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn tiers_run_always_then_action_then_kind_in_registration_order() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(EventKind::PullRequest, record_event(&calls, "kind-1"))
            .always(record(&calls, "always-1"))
            .on(
                (EventKind::PullRequest, Action::Opened),
                record_event(&calls, "action-1"),
            )
            .on(EventKind::PullRequest, record_event(&calls, "kind-2"))
            .always(record(&calls, "always-2"))
            .on(
                (EventKind::PullRequest, Action::Opened),
                record_event(&calls, "action-2"),
            )
            .fallback(record(&calls, "fallback"))
            .build();

        dispatcher.dispatch(pull_request_opened()).await.unwrap();

        assert_eq!(
            calls.lock().await.as_slice(),
            [
                "always-1", "always-2", "action-1", "action-2", "kind-1", "kind-2"
            ]
        );
    }

    #[tokio::test]
    async fn payload_routes_run_the_action_chain_before_the_kind_chain_in_registration_order() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(record_payload(&calls, "kind-1"))
            .on_payload_action([Action::Opened], record_payload(&calls, "action-1"))
            .on_payload(record_payload(&calls, "kind-2"))
            .on_payload_action([Action::Opened], record_payload(&calls, "action-2"))
            .build();

        dispatcher.dispatch(pull_request_opened()).await.unwrap();

        assert_eq!(
            calls.lock().await.as_slice(),
            ["action-1", "action-2", "kind-1", "kind-2"]
        );
    }

    #[tokio::test]
    async fn tiers_run_raw_then_always_then_routes_then_fallback_in_registration_order() {
        // Registration order is interleaved across tiers on purpose: the tier
        // decides when a handler runs, and only order within a tier follows
        // registration.
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(record(&calls, "always-1"))
            .on_payload(record_payload(&calls, "route-1"))
            .always_raw(record_envelope(&calls, "raw-1"))
            .fallback(record(&calls, "fallback-1"))
            .on_payload(record_payload(&calls, "route-2"))
            .always(record(&calls, "always-2"))
            .always_raw(record_envelope(&calls, "raw-2"))
            .fallback(record(&calls, "fallback-2"))
            .build();

        dispatcher.dispatch(pull_request_opened()).await.unwrap();
        assert_eq!(
            calls.lock().await.as_slice(),
            [
                "raw-1", "raw-2", "always-1", "always-2", "route-1", "route-2"
            ]
        );

        calls.lock().await.clear();
        dispatcher.dispatch(check_run_completed()).await.unwrap();
        assert_eq!(
            calls.lock().await.as_slice(),
            [
                "raw-1",
                "raw-2",
                "always-1",
                "always-2",
                "fallback-1",
                "fallback-2"
            ]
        );
    }

    #[tokio::test]
    async fn always_runs_for_every_delivery_without_counting_as_a_match() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(record(&calls, "audit"))
            .on_payload(record_payload(&calls, "pull-request"))
            .fallback(fail(&calls, "unmatched"))
            .build();

        // Only `always` applies: the strict fallback still rejects it.
        assert_eq!(
            dispatcher.dispatch(installation_created()).await,
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["audit", "unmatched"]);
    }

    #[tokio::test]
    async fn the_raw_tier_runs_for_every_delivery_without_counting_as_a_match() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always_raw(record_envelope(&calls, "persist"))
            .on_payload(record_payload(&calls, "pull-request"))
            .fallback(fail(&calls, "unmatched"))
            .build();

        // Persisting the envelope says nothing about whether any route claims
        // its kind: the strict fallback still rejects it.
        assert_eq!(
            dispatcher.dispatch(installation_created()).await,
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["persist", "unmatched"]);
    }

    #[tokio::test]
    async fn always_runs_for_a_payload_octocrab_cannot_represent() {
        let calls = Calls::default();
        let handler_calls = Arc::clone(&calls);
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(move |meta: EventMeta| {
                let calls = Arc::clone(&handler_calls);
                async move {
                    calls.lock().await.push("audit");
                    assert_eq!(meta.kind, EventKind::PullRequest);
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .build();

        // The always tier receives only the metadata, so nothing is decoded
        // and the delivery succeeds although octocrab cannot represent it.
        assert_eq!(dispatcher.dispatch(unrepresentable()).await, Ok(()));
        assert_eq!(calls.lock().await.as_slice(), ["audit"]);
    }

    #[tokio::test]
    async fn a_strict_fallback_reports_its_own_error_for_an_unmatched_unrepresentable_payload() {
        #[derive(serde::Deserialize)]
        struct AnyCheckRun {}
        crate::impl_payload!(AnyCheckRun => EventKind::CheckRun);

        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(|_: EventMeta, _: AnyCheckRun| async {
                Ok::<_, std::convert::Infallible>(())
            })
            .fallback(fail(&calls, "unmatched"))
            .build();

        // Nothing routes `pull_request`, so the fallback decides: it never
        // decodes, so the answer is "unhandled", not a decode error.
        assert_eq!(
            dispatcher.dispatch(unrepresentable()).await,
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["unmatched"]);
    }

    #[tokio::test]
    async fn the_fallback_chain_runs_in_order_only_when_nothing_matched() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(record_payload(&calls, "pull-request"))
            .fallback(record(&calls, "log"))
            .fallback(fail(&calls, "reject"))
            .build();

        assert_eq!(
            dispatcher.dispatch(check_run_completed()).await,
            Err(AppError::Handler("reject"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["log", "reject"]);

        calls.lock().await.clear();
        dispatcher.dispatch(pull_request_opened()).await.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["pull-request"]);
    }

    #[tokio::test]
    async fn unmatched_deliveries_succeed_when_the_fallback_chain_is_empty() {
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(|_: EventMeta, _: AnyPullRequest| async { Ok::<_, AppError>(()) })
            .build();

        assert_eq!(dispatcher.dispatch(unknown()).await, Ok(()));
        assert_eq!(dispatcher.dispatch(ping()).await, Ok(()));
        assert_eq!(dispatcher.dispatch(check_run_completed()).await, Ok(()));
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn a_registered_kind_with_no_matching_action_still_falls_back() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                (EventKind::PullRequest, Action::Closed),
                record_event(&calls, "closed"),
            )
            .fallback(fail(&calls, "unmatched"))
            .build();

        assert_eq!(
            dispatcher.dispatch(pull_request_opened()).await,
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["unmatched"]);
    }

    #[tokio::test]
    async fn a_payload_handler_registered_for_some_actions_matches_only_those() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload_action(
                [Action::Opened, Action::Reopened],
                record_payload(&calls, "triage"),
            )
            .fallback(fail(&calls, "unmatched"))
            .build();

        dispatcher.dispatch(pull_request_opened()).await.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["triage"]);

        calls.lock().await.clear();
        dispatcher
            .dispatch(pull_request(Action::Reopened))
            .await
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["triage"]);

        // `closed` was never registered, so the kind alone earns no match:
        // the strict fallback rejects it instead of the handler silently
        // widening to every pull-request action.
        calls.lock().await.clear();
        assert_eq!(
            dispatcher.dispatch(pull_request(Action::Closed)).await,
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["unmatched"]);
    }

    #[tokio::test]
    async fn an_unregistered_action_is_not_decoded() {
        // A view the payload below cannot satisfy: had the route decoded, the
        // delivery would have failed with a decode error.
        #[derive(serde::Deserialize)]
        struct Number {
            #[allow(
                dead_code,
                reason = "the field is required so the decode fails; nothing reads it"
            )]
            number: u64,
        }
        crate::impl_payload!(Number => EventKind::PullRequest);

        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload_action([Action::Opened], |_: EventMeta, _: Number| async {
                Ok::<_, std::convert::Infallible>(())
            })
            .fallback(fail(&calls, "unmatched"))
            .build();

        // An action this crate does not know yet: nothing registered cares,
        // so nothing is decoded, and the fallback answers.
        let future = envelope_with_action(
            EventKind::PullRequest,
            Action::Unknown("future_action".into()),
            br#"{"action":"future_action"}"#,
        );
        assert_eq!(
            dispatcher.dispatch(future).await,
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["unmatched"]);
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn an_action_octocrab_does_not_know_fails_the_delivery_only_at_a_handler_that_needs_it() {
        use octocrab::models::webhook_events::payload::PullRequestWebhookEventPayload;

        // octocrab's per-kind action enums have no catch-all, so its payload
        // struct cannot decode an action it does not know. A handler over it
        // registered for the actions it wants is never asked to; a consumer
        // view registered kind-wide still runs.
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload_action(
                [Action::Opened],
                |_: EventMeta, _: PullRequestWebhookEventPayload| async {
                    Ok::<_, std::convert::Infallible>(())
                },
            )
            .on_payload(record_payload(&calls, "view"))
            .build();

        let future = envelope_with_action(
            EventKind::PullRequest,
            Action::Unknown("future_action".into()),
            br#"{"action":"future_action","number":2}"#,
        );
        assert_eq!(dispatcher.dispatch(future.clone()).await, Ok(()));
        assert_eq!(calls.lock().await.as_slice(), ["view"]);

        // The same handler registered for every action of the kind is asked,
        // and the delivery fails at its decode.
        let kind_wide = Dispatcher::<AppError>::builder()
            .on_payload(|_: EventMeta, _: PullRequestWebhookEventPayload| async {
                Ok::<_, std::convert::Infallible>(())
            })
            .build();
        assert_eq!(kind_wide.dispatch(future).await, Err(AppError::Decode));
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn every_matcher_form_expands_to_its_routes() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                [EventKind::PullRequest, EventKind::CheckRun],
                record_event(&calls, "kinds"),
            )
            .on(
                (EventKind::PullRequest, [Action::Opened, Action::Closed]),
                record_event(&calls, "actions"),
            )
            .on(
                [
                    (EventKind::PullRequest, Action::Opened),
                    (EventKind::CheckRun, Action::Completed),
                ],
                record_event(&calls, "pairs"),
            )
            .on(
                EventMatcher::from(EventKind::Installation)
                    .or((EventKind::CheckRun, Action::Completed)),
                record_event(&calls, "or"),
            )
            .build();

        dispatcher.dispatch(pull_request_opened()).await.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["actions", "pairs", "kinds"]);

        calls.lock().await.clear();
        dispatcher.dispatch(check_run_completed()).await.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["pairs", "or", "kinds"]);

        calls.lock().await.clear();
        dispatcher.dispatch(installation_created()).await.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["or"]);

        // The action list is exact: a pull request being synchronized does not
        // reach the [opened, closed] handler.
        calls.lock().await.clear();
        let synchronized = envelope_with_action(
            EventKind::PullRequest,
            Action::Synchronize,
            include_bytes!("../tests/fixtures/pull_request.opened.json"),
        );
        dispatcher.dispatch(synchronized).await.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["kinds"]);
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn on_payload_routes_a_payload_handler_by_its_payload_type() {
        use octocrab::models::webhook_events::payload::{
            PullRequestWebhookEventAction, PullRequestWebhookEventPayload,
        };

        struct Labeler {
            seen: Arc<Mutex<Vec<(String, u64, PullRequestWebhookEventAction)>>>,
        }

        impl crate::PayloadHandler<PullRequestWebhookEventPayload> for Labeler {
            type Error = std::convert::Infallible;

            async fn handle(
                &self,
                meta: EventMeta,
                payload: PullRequestWebhookEventPayload,
            ) -> Result<(), Self::Error> {
                self.seen
                    .lock()
                    .await
                    .push((meta.delivery_id, payload.number, payload.action));
                Ok(())
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(Labeler {
                seen: Arc::clone(&seen),
            })
            .build();

        dispatcher.dispatch(pull_request_opened()).await.unwrap();
        assert_eq!(
            seen.lock().await.as_slice(),
            [(
                "delivery".to_owned(),
                2,
                PullRequestWebhookEventAction::Opened
            )]
        );

        // Another kind never reaches it, and with no fallback still succeeds.
        dispatcher.dispatch(check_run_completed()).await.unwrap();
        assert_eq!(seen.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn on_payload_accepts_a_consumer_defined_payload_view() {
        #[derive(serde::Deserialize)]
        struct Conclusion {
            check_run: CheckRunConclusion,
        }

        #[derive(serde::Deserialize)]
        struct CheckRunConclusion {
            conclusion: String,
        }

        crate::impl_payload!(Conclusion => EventKind::CheckRun);

        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(move |meta: EventMeta, payload: Conclusion| {
                let seen = Arc::clone(&handler_seen);
                async move {
                    seen.lock()
                        .await
                        .push((meta.delivery_id, payload.check_run.conclusion));
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .build();

        dispatcher.dispatch(check_run_completed()).await.unwrap();

        assert_eq!(
            seen.lock().await.as_slice(),
            [("delivery".to_owned(), "success".to_owned())]
        );
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn each_handlers_error_converts_into_the_dispatcher_error() {
        use octocrab::models::webhook_events::payload::PullRequestWebhookEventPayload;

        #[derive(Debug, PartialEq)]
        struct DbError;
        #[derive(Debug, PartialEq)]
        struct ApiError;
        #[derive(Debug, PartialEq)]
        struct QueueError;

        #[derive(Debug, PartialEq)]
        enum ServiceError {
            Decode,
            Db(DbError),
            Api(ApiError),
            Queue(QueueError),
        }

        impl From<DecodeError> for ServiceError {
            fn from(_: DecodeError) -> Self {
                Self::Decode
            }
        }
        impl From<DbError> for ServiceError {
            fn from(error: DbError) -> Self {
                Self::Db(error)
            }
        }
        impl From<ApiError> for ServiceError {
            fn from(error: ApiError) -> Self {
                Self::Api(error)
            }
        }
        impl From<QueueError> for ServiceError {
            fn from(error: QueueError) -> Self {
                Self::Queue(error)
            }
        }

        // One dispatcher, three handlers, three error types.
        let dispatcher = Dispatcher::<ServiceError>::builder()
            .always(|meta: EventMeta| async move {
                if meta.kind == EventKind::Installation {
                    Err(DbError)
                } else {
                    Ok(())
                }
            })
            .on(EventKind::CheckRun, |_: EventMeta, _: WebhookEvent| async {
                Err::<(), _>(ApiError)
            })
            .on_payload(|_: EventMeta, _: PullRequestWebhookEventPayload| async {
                Err::<(), _>(QueueError)
            })
            .build();

        assert_eq!(
            dispatcher.dispatch(installation_created()).await,
            Err(ServiceError::Db(DbError))
        );
        assert_eq!(
            dispatcher.dispatch(check_run_completed()).await,
            Err(ServiceError::Api(ApiError))
        );
        assert_eq!(
            dispatcher.dispatch(pull_request_opened()).await,
            Err(ServiceError::Queue(QueueError))
        );
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn an_event_decode_failure_stops_the_delivery_at_the_first_event_handler() {
        // The always tier and payload handlers over a consumer view sit either
        // side of the event handler, so the log shows exactly where the chain
        // stopped: octocrab decodes once, when the first event handler is
        // reached, and nothing before it needed octocrab.
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(record(&calls, "always"))
            .on_payload(record_payload(&calls, "payload-before"))
            .on(EventKind::PullRequest, record_event(&calls, "event"))
            .on_payload(record_payload(&calls, "payload-after"))
            .on(EventKind::PullRequest, record_event(&calls, "event-after"))
            .fallback(fail(&calls, "unmatched"))
            .build();

        assert_eq!(
            dispatcher.dispatch(unrepresentable()).await,
            Err(AppError::Decode)
        );
        assert_eq!(calls.lock().await.as_slice(), ["always", "payload-before"]);
    }

    #[tokio::test]
    async fn a_delivery_with_only_raw_meta_and_payload_handlers_never_decodes_the_event() {
        // The same unrepresentable payload succeeds when no event handler
        // needs octocrab's decoding: the raw tier sees bytes, the meta tiers
        // see metadata, and a consumer view over the bytes has nothing
        // octocrab must represent.
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always_raw(record_envelope(&calls, "raw"))
            .always(record(&calls, "always"))
            .on_payload(record_payload(&calls, "payload"))
            .fallback(fail(&calls, "unmatched"))
            .build();

        assert_eq!(dispatcher.dispatch(unrepresentable()).await, Ok(()));
        assert_eq!(calls.lock().await.as_slice(), ["raw", "always", "payload"]);
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn the_raw_tier_runs_before_an_event_handler_fails_to_decode() {
        // Persist-before-route holds for a payload octocrab cannot represent:
        // the raw tier stores it, and the decode failure that fails the
        // delivery is reported at the event handler, never earlier.
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always_raw(record_envelope(&calls, "raw"))
            .on(EventKind::PullRequest, record_event(&calls, "event"))
            .build();

        assert_eq!(
            dispatcher.dispatch(unrepresentable()).await,
            Err(AppError::Decode)
        );
        assert_eq!(calls.lock().await.as_slice(), ["raw"]);
    }

    #[tokio::test]
    async fn a_failure_in_the_raw_tier_fails_the_delivery_before_any_later_tier() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always_raw(fail_envelope(&calls, "persist"))
            .always_raw(record_envelope(&calls, "persist-after"))
            .always(record(&calls, "audit"))
            .on_payload(record_payload(&calls, "routed"))
            .fallback(record(&calls, "fallback"))
            .build();

        // A delivery that could not be persisted is not routed: the raw chain
        // stops at the failure, and neither the always tier, the matching
        // route, nor the fallback sees it.
        assert_eq!(
            dispatcher.dispatch(pull_request_opened()).await,
            Err(AppError::Handler("persist"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["persist"]);
    }

    #[tokio::test]
    async fn the_always_and_fallback_chains_fail_fast() {
        let calls = Calls::default();
        let always = Dispatcher::<AppError>::builder()
            .always(fail(&calls, "always"))
            .always(record(&calls, "always-after"))
            .on_payload(record_payload(&calls, "routed"))
            .build();
        assert_eq!(
            always.dispatch(pull_request_opened()).await,
            Err(AppError::Handler("always"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["always"]);

        calls.lock().await.clear();
        let fallback = Dispatcher::<AppError>::builder()
            .fallback(fail(&calls, "fallback"))
            .fallback(record(&calls, "fallback-after"))
            .build();
        assert_eq!(
            fallback.dispatch(pull_request_opened()).await,
            Err(AppError::Handler("fallback"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["fallback"]);
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn the_routed_chains_fail_fast() {
        let calls = Calls::default();
        let routed = Dispatcher::<AppError>::builder()
            .on(
                (EventKind::PullRequest, Action::Opened),
                fail_event(&calls, "action"),
            )
            .on(
                (EventKind::PullRequest, Action::Opened),
                record_event(&calls, "action-after"),
            )
            .on(EventKind::PullRequest, record_event(&calls, "kind"))
            .build();
        assert_eq!(
            routed.dispatch(pull_request_opened()).await,
            Err(AppError::Handler("action"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["action"]);

        calls.lock().await.clear();
        let kind_wide = Dispatcher::<AppError>::builder()
            .on(EventKind::PullRequest, fail_event(&calls, "kind"))
            .on(EventKind::PullRequest, record_event(&calls, "kind-after"))
            .build();
        assert_eq!(
            kind_wide.dispatch(pull_request_opened()).await,
            Err(AppError::Handler("kind"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["kind"]);
    }

    #[tokio::test]
    async fn a_struct_handling_two_payloads_is_registered_with_a_turbofish() {
        use crate::PayloadHandler;

        #[derive(serde::Deserialize)]
        struct AnyCheckRun {}
        crate::impl_payload!(AnyCheckRun => EventKind::CheckRun);

        struct Labeler {
            calls: Calls,
        }

        impl PayloadHandler<AnyPullRequest> for Labeler {
            type Error = AppError;

            async fn handle(&self, _: EventMeta, _: AnyPullRequest) -> Result<(), AppError> {
                self.calls.lock().await.push("pull-request");
                Ok(())
            }
        }

        impl PayloadHandler<AnyCheckRun> for Labeler {
            type Error = AppError;

            async fn handle(&self, _: EventMeta, _: AnyCheckRun) -> Result<(), AppError> {
                self.calls.lock().await.push("check-run");
                Ok(())
            }
        }

        let calls = Calls::default();
        // The struct alone no longer says which payload is meant, so each
        // registration names it. The dispatcher `Arc`s each registration;
        // the caller shares nothing by hand.
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload::<AnyPullRequest, _>(Labeler {
                calls: Arc::clone(&calls),
            })
            .on_payload::<AnyCheckRun, _>(Labeler {
                calls: Arc::clone(&calls),
            })
            .build();

        dispatcher.dispatch(pull_request_opened()).await.unwrap();
        dispatcher.dispatch(check_run_completed()).await.unwrap();

        assert_eq!(calls.lock().await.as_slice(), ["pull-request", "check-run"]);
    }

    #[test]
    fn debug_prints_the_route_table_with_handlers_elided() {
        // Neither the error type nor any handler is `Debug`; the table of
        // kinds, actions and handler flavours is what prints.
        struct NotDebug;
        impl From<DecodeError> for NotDebug {
            fn from(_: DecodeError) -> Self {
                Self
            }
        }

        let builder = Dispatcher::<NotDebug>::builder()
            .always_raw(|_: Envelope| async { Ok::<_, NotDebug>(()) })
            .always(|_: EventMeta| async { Ok::<_, NotDebug>(()) })
            .on_payload(|_: EventMeta, _: AnyPullRequest| async { Ok::<_, NotDebug>(()) })
            .fallback(|_: EventMeta| async { Ok::<_, NotDebug>(()) });

        let debug = format!("{builder:?}");
        assert!(debug.starts_with("DispatcherBuilder {"), "{debug}");
        assert!(debug.contains("raw: [Raw(..)]"), "{debug}");
        assert!(debug.contains("always: [Meta(..)]"), "{debug}");
        assert!(
            debug.contains("PullRequest: KindRoutes { any_action: [Payload(..)], by_action: {} }"),
            "{debug}"
        );
        assert!(debug.contains("fallback: [Meta(..)]"), "{debug}");

        let debug = format!("{:?}", builder.build());
        assert!(debug.starts_with("Dispatcher {"), "{debug}");
        assert!(debug.contains("raw: [Raw(..)]"), "{debug}");
        assert!(debug.contains("always: [Meta(..)]"), "{debug}");
    }

    #[cfg(feature = "octocrab")]
    #[test]
    fn debug_shows_event_routes_by_action() {
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                (EventKind::PullRequest, Action::Opened),
                |_: EventMeta, _: WebhookEvent| async { Ok::<_, AppError>(()) },
            )
            .on(
                (EventKind::PullRequest, Action::Opened),
                |_: EventMeta, _: WebhookEvent| async { Ok::<_, AppError>(()) },
            )
            .build();

        let debug = format!("{dispatcher:?}");
        assert!(
            debug.contains(
                "PullRequest: KindRoutes { any_action: [], by_action: {Opened: [Event(..), Event(..)]} }"
            ),
            "{debug}"
        );
    }
}
