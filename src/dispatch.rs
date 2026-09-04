use std::{collections::HashMap, future::Future, sync::Arc};

use octocrab::models::webhook_events::WebhookEvent;

use crate::{
    Action, DecodeError, Envelope, EventHandler, EventKind, EventMatcher, EventMeta, MaybeSend,
    MaybeSync, Payload, PayloadHandler, WebhookHandler, matcher::Slot, runtime::BoxFuture, trace,
};

// Erased handlers. A trait object admits only one non-auto trait, so these
// cannot be written as `dyn Fn(..) + MaybeSend + MaybeSync` and carry the
// platform split by hand; see `runtime` for the rationale.
#[cfg(not(target_arch = "wasm32"))]
type EventFn<E> =
    Arc<dyn Fn(EventMeta, WebhookEvent) -> BoxFuture<Result<(), E>> + Send + Sync + 'static>;
#[cfg(target_arch = "wasm32")]
type EventFn<E> = Arc<dyn Fn(EventMeta, WebhookEvent) -> BoxFuture<Result<(), E>> + 'static>;

#[cfg(not(target_arch = "wasm32"))]
type EnvelopeFn<E> = Arc<dyn Fn(Envelope) -> BoxFuture<Result<(), E>> + Send + Sync + 'static>;
#[cfg(target_arch = "wasm32")]
type EnvelopeFn<E> = Arc<dyn Fn(Envelope) -> BoxFuture<Result<(), E>> + 'static>;

/// A handler that routes verified envelopes to typed handlers by kind and
/// action.
///
/// Every registration is typed: `always`, `on`, and `fallback` take an
/// [`EventHandler`], and `handle_with` takes a [`PayloadHandler`] whose kind
/// comes from its payload type. Each handler keeps its own error type; the
/// dispatcher converts them into `E` through `From` at registration.
///
/// Per delivery the dispatcher runs the `always` chain, then the chain for
/// the envelope's kind and action, then the kind-wide chain, and the
/// `fallback` chain only if neither routed chain matched. Every chain is
/// sequential, in registration order, and stops at the first error. The
/// `always` chain never counts as a match, and an empty fallback chain
/// succeeds, so unhandled kinds are green in GitHub until you decide
/// otherwise.
///
/// octocrab's [`WebhookEvent`] is decoded at most once per delivery, when the
/// first event handler is reached, and shared with the rest; a payload it
/// cannot represent fails the delivery at that position. Payload handlers
/// decode their own type from the raw bytes.
///
/// ```
/// use octocrab::models::webhook_events::{WebhookEvent, payload::PullRequestWebhookEventPayload};
/// use octoevents::{Action, DecodeError, Dispatcher, EventKind, EventMeta};
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
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .always(|meta: EventMeta, _: WebhookEvent| async move {
///         println!("{} {}", meta.delivery_id, meta.kind);
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .on((EventKind::PullRequest, [Action::Opened, Action::Synchronize]), |meta: EventMeta, event: WebhookEvent| async move {
///         println!("triage {:?} for {:?}", meta.action, event.repository.map(|repository| repository.name));
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .handle_with(|_: EventMeta, payload: PullRequestWebhookEventPayload| async move {
///         println!("label PR #{}", payload.number);
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .fallback(|meta: EventMeta, _: WebhookEvent| async move {
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
/// No method accepts a [`WebhookHandler`]: work on the raw bytes (persist,
/// forward) wraps the dispatcher in a webhook handler instead, so the
/// envelope is stored before any typed handler runs.
///
/// Enabling the `octocrab` feature makes octocrab's pre-1.0 version part of
/// this crate's public API, and the dispatcher is built on it: an octocrab
/// major bump is a breaking change for this type.
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

impl<E> Dispatcher<E>
where
    E: From<DecodeError> + 'static,
{
    /// Starts building a dispatcher whose unmatched deliveries succeed.
    #[must_use]
    pub fn builder() -> DispatcherBuilder<E> {
        DispatcherBuilder::default()
    }

    /// Runs the `always` chain, the matching routed chains, and the fallback
    /// chain when nothing matched, in that order.
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
            event: None,
        };

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

/// One envelope being dispatched: the envelope plus the lazily decoded event
/// that every event route shares.
struct InFlight<'a> {
    envelope: &'a Envelope,
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
            Route::Payload(handler) => handler(self.envelope.clone()).await,
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

impl<E> Default for DispatcherBuilder<E> {
    fn default() -> Self {
        Self {
            routes: Routes {
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
    /// Registers an event handler that runs for every delivery, before
    /// routing.
    ///
    /// The place for audit, metrics, and deduplication: its failure fails the
    /// delivery, and it never counts as a match, so a strict fallback still
    /// rejects kinds nothing else handles.
    #[must_use]
    pub fn always<H>(mut self, handler: H) -> Self
    where
        H: EventHandler + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.routes.always.push(event_route(handler));
        self
    }

    /// Registers an event handler for the kinds and actions the matcher
    /// selects.
    ///
    /// A handler registered under several slots is shared, not duplicated.
    #[must_use]
    pub fn on<H>(mut self, matcher: impl Into<EventMatcher>, handler: H) -> Self
    where
        H: EventHandler + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        let route = event_route(handler);
        for slot in matcher.into().into_slots() {
            self.insert(slot, route.clone());
        }
        self
    }

    /// Registers a payload handler for the kind its payload type declares.
    ///
    /// No matcher is needed, and none is accepted: the kind is `P::KIND`, so a
    /// pull-request handler cannot end up under `issues`. Filter on the
    /// action inside the handler, using the payload's typed `action` field.
    #[must_use]
    pub fn handle_with<P, H>(mut self, handler: H) -> Self
    where
        P: Payload + 'static,
        H: PayloadHandler<P> + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.insert(Slot::any_action(P::KIND), payload_route(handler));
        self
    }

    /// Appends an event handler to the chain that runs when no routed chain
    /// matched.
    ///
    /// Several may be registered; they run in order and stop at the first
    /// error. "Log it, then reject it" is two small handlers.
    #[must_use]
    pub fn fallback<H>(mut self, handler: H) -> Self
    where
        H: EventHandler + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.routes.fallback.push(event_route(handler));
        self
    }

    /// Finishes the dispatcher.
    #[must_use]
    pub fn build(self) -> Dispatcher<E> {
        Dispatcher {
            routes: Arc::new(self.routes),
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
            // Routing already guaranteed the kind, so only the shape of the
            // payload can still disagree.
            let payload = envelope.decode_payload::<P>().map_err(E::from)?;
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
    /// Takes the shared decoded event.
    Event(EventFn<E>),
    /// Takes the envelope and decodes its own payload type.
    Payload(EnvelopeFn<E>),
}

impl<E> Clone for Route<E> {
    fn clone(&self) -> Self {
        match self {
            Self::Event(handler) => Self::Event(Arc::clone(handler)),
            Self::Payload(handler) => Self::Payload(Arc::clone(handler)),
        }
    }
}

/// Every chain a dispatcher can run.
struct Routes<E> {
    always: Vec<Route<E>>,
    by_kind: HashMap<EventKind, KindRoutes<E>>,
    fallback: Vec<Route<E>>,
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::{future::Future, pin::Pin, sync::Arc};

    use octocrab::models::webhook_events::WebhookEvent;
    use tokio::sync::Mutex;

    use super::Dispatcher;
    use crate::{
        Action, DecodeError, EventKind, EventMatcher, EventMeta,
        test_support::{
            AppError, check_run_completed, envelope_with_action, installation_created, ping,
            pull_request_opened, unknown, unrepresentable,
        },
    };

    type Calls = Arc<Mutex<Vec<&'static str>>>;
    type Recorded<E> = Pin<Box<dyn Future<Output = Result<(), E>> + Send>>;

    /// An event handler that appends `value` to the shared log.
    fn record(
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
    fn fail(
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

    #[tokio::test]
    async fn tiers_run_always_then_action_then_kind_in_registration_order() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(EventKind::PullRequest, record(&calls, "kind-1"))
            .always(record(&calls, "always-1"))
            .on(
                (EventKind::PullRequest, Action::Opened),
                record(&calls, "action-1"),
            )
            .on(EventKind::PullRequest, record(&calls, "kind-2"))
            .always(record(&calls, "always-2"))
            .on(
                (EventKind::PullRequest, Action::Opened),
                record(&calls, "action-2"),
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
    async fn always_runs_for_every_delivery_without_counting_as_a_match() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(record(&calls, "audit"))
            .on(EventKind::PullRequest, record(&calls, "pull-request"))
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
    async fn the_fallback_chain_runs_in_order_only_when_nothing_matched() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(EventKind::PullRequest, record(&calls, "pull-request"))
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
            .on(
                EventKind::PullRequest,
                |_: EventMeta, _: WebhookEvent| async { Ok::<_, AppError>(()) },
            )
            .build();

        assert_eq!(dispatcher.dispatch(unknown()).await, Ok(()));
        assert_eq!(dispatcher.dispatch(ping()).await, Ok(()));
        assert_eq!(dispatcher.dispatch(check_run_completed()).await, Ok(()));
    }

    #[tokio::test]
    async fn a_registered_kind_with_no_matching_action_still_falls_back() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                (EventKind::PullRequest, Action::Closed),
                record(&calls, "closed"),
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
    async fn every_matcher_form_expands_to_its_routes() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                [EventKind::PullRequest, EventKind::CheckRun],
                record(&calls, "kinds"),
            )
            .on(
                (EventKind::PullRequest, [Action::Opened, Action::Closed]),
                record(&calls, "actions"),
            )
            .on(
                [
                    (EventKind::PullRequest, Action::Opened),
                    (EventKind::CheckRun, Action::Completed),
                ],
                record(&calls, "pairs"),
            )
            .on(
                EventMatcher::from(EventKind::Installation)
                    .or((EventKind::CheckRun, Action::Completed)),
                record(&calls, "or"),
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

    #[tokio::test]
    async fn handle_with_routes_a_payload_handler_by_its_payload_type() {
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
            .handle_with(Labeler {
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
    async fn handle_with_accepts_a_consumer_defined_payload_view() {
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
            .handle_with(move |meta: EventMeta, payload: Conclusion| {
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
            .always(|meta: EventMeta, _: WebhookEvent| async move {
                if meta.kind == EventKind::Installation {
                    Err(DbError)
                } else {
                    Ok(())
                }
            })
            .on(EventKind::CheckRun, |_: EventMeta, _: WebhookEvent| async {
                Err::<(), _>(ApiError)
            })
            .handle_with(|_: EventMeta, _: PullRequestWebhookEventPayload| async {
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

    #[tokio::test]
    async fn an_event_decode_failure_stops_the_delivery_at_the_first_event_handler() {
        // Payload handlers over a consumer view sit either side of the event
        // handler, so the log shows exactly where the chain stopped.
        #[derive(serde::Deserialize)]
        struct Anything {}
        crate::impl_payload!(Anything => EventKind::PullRequest);

        let calls = Calls::default();
        let payload_record = |value: &'static str| {
            let calls = Arc::clone(&calls);
            move |_: EventMeta, _: Anything| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.lock().await.push(value);
                    Ok::<_, std::convert::Infallible>(())
                }
            }
        };

        let dispatcher = Dispatcher::<AppError>::builder()
            .handle_with(payload_record("payload-before"))
            .on(EventKind::PullRequest, record(&calls, "event"))
            .handle_with(payload_record("payload-after"))
            .on(EventKind::PullRequest, record(&calls, "event-after"))
            .build();

        assert_eq!(
            dispatcher.dispatch(unrepresentable()).await,
            Err(AppError::Decode)
        );
        assert_eq!(calls.lock().await.as_slice(), ["payload-before"]);
    }

    #[tokio::test]
    async fn a_delivery_with_only_payload_handlers_never_decodes_the_event() {
        // The same unrepresentable payload succeeds when no event handler
        // needs octocrab's decoding, including in the `always` tier's absence.
        #[derive(serde::Deserialize)]
        struct Anything {}
        crate::impl_payload!(Anything => EventKind::PullRequest);

        let dispatcher = Dispatcher::<AppError>::builder()
            .handle_with(|_: EventMeta, _: Anything| async {
                Ok::<_, std::convert::Infallible>(())
            })
            .build();

        assert_eq!(dispatcher.dispatch(unrepresentable()).await, Ok(()));
    }

    #[tokio::test]
    async fn every_chain_fails_fast() {
        let calls = Calls::default();
        let always = Dispatcher::<AppError>::builder()
            .always(fail(&calls, "always"))
            .always(record(&calls, "always-after"))
            .on(EventKind::PullRequest, record(&calls, "routed"))
            .build();
        assert_eq!(
            always.dispatch(pull_request_opened()).await,
            Err(AppError::Handler("always"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["always"]);

        calls.lock().await.clear();
        let routed = Dispatcher::<AppError>::builder()
            .on(
                (EventKind::PullRequest, Action::Opened),
                fail(&calls, "action"),
            )
            .on(
                (EventKind::PullRequest, Action::Opened),
                record(&calls, "action-after"),
            )
            .on(EventKind::PullRequest, record(&calls, "kind"))
            .build();
        assert_eq!(
            routed.dispatch(pull_request_opened()).await,
            Err(AppError::Handler("action"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["action"]);

        calls.lock().await.clear();
        let kind_wide = Dispatcher::<AppError>::builder()
            .on(EventKind::PullRequest, fail(&calls, "kind"))
            .on(EventKind::PullRequest, record(&calls, "kind-after"))
            .build();
        assert_eq!(
            kind_wide.dispatch(pull_request_opened()).await,
            Err(AppError::Handler("kind"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["kind"]);

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
}
