use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use crate::{Action, Envelope, EventKind, MaybeSend, MaybeSync};

/// A handler accepted by `WebhookReceiver` and implemented by [`Dispatcher`].
///
/// `Clone` is deliberately not a supertrait. It is required only where a
/// delivery must own its handler: `WebhookReceiver::receive` and the `tower`
/// `Service` impl clone the receiver per delivery so their futures can be
/// `Send` without demanding `H: Sync`, and those paths state the bound
/// themselves. A transport that drives `handle` by reference needs no
/// `Clone` at all.
pub trait WebhookHandler<E> {
    /// The future returned by this handler.
    type Future: Future<Output = Result<(), E>>;

    /// Handles one verified envelope.
    fn handle(&self, envelope: Envelope) -> Self::Future;
}

impl<E, F, Fut> WebhookHandler<E> for F
where
    F: Fn(Envelope) -> Fut,
    Fut: Future<Output = Result<(), E>>,
{
    type Future = Fut;

    fn handle(&self, envelope: Envelope) -> Self::Future {
        self(envelope)
    }
}

#[cfg(not(target_arch = "wasm32"))]
type BoxFuture<E> = Pin<Box<dyn Future<Output = Result<(), E>> + Send + 'static>>;
#[cfg(target_arch = "wasm32")]
type BoxFuture<E> = Pin<Box<dyn Future<Output = Result<(), E>> + 'static>>;

#[cfg(not(target_arch = "wasm32"))]
type BoxHandler<E> = Arc<dyn Fn(Envelope) -> BoxFuture<E> + Send + Sync + 'static>;
#[cfg(target_arch = "wasm32")]
type BoxHandler<E> = Arc<dyn Fn(Envelope) -> BoxFuture<E> + 'static>;

/// A cloneable, feature-free router for verified webhook envelopes.
///
/// An optional handler implementation, not a framework: an action-specific
/// chain runs before the kind-wide chain, each chain is sequential,
/// registration-ordered, and fail-fast, and unmatched events succeed unless
/// [`DispatcherBuilder::fallback`] says otherwise.
///
/// ```
/// use octoevents::{Action, Dispatcher, EventKind};
///
/// let dispatcher = Dispatcher::builder()
///     .on_action(EventKind::PullRequest, Action::Closed, |envelope| async move {
///         let payload: serde_json::Value = envelope.parse()?;
///         println!("closed PR: {payload}");
///         Ok::<_, serde_json::Error>(())
///     })
///     .build();
/// # let _ = dispatcher;
/// ```
pub struct Dispatcher<E> {
    handlers: Arc<HashMap<EventKind, KindRoutes<E>>>,
    fallback: BoxHandler<E>,
}

impl<E> Clone for Dispatcher<E> {
    fn clone(&self) -> Self {
        Self {
            handlers: Arc::clone(&self.handlers),
            fallback: Arc::clone(&self.fallback),
        }
    }
}

impl<E> Dispatcher<E>
where
    E: 'static,
{
    /// Starts building a dispatcher whose unmatched events succeed.
    #[must_use]
    pub fn builder() -> DispatcherBuilder<E> {
        DispatcherBuilder::default()
    }

    /// Runs matching handlers sequentially, action-specific before kind-wide.
    ///
    /// # Errors
    ///
    /// Stops and returns the first handler error.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            name = "octoevents.dispatch",
            skip_all,
            fields(delivery_id = %envelope.delivery_id, event = %envelope.kind, outcome = tracing::field::Empty)
        )
    )]
    pub async fn dispatch(&self, envelope: Envelope) -> Result<(), E> {
        // Routes are keyed by kind first so a delivery is looked up entirely by
        // reference: no EventKind or Action is cloned to build a lookup key.
        let routes = self.handlers.get(&envelope.kind);
        let specific = routes.and_then(|routes| {
            envelope
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
            for handler in chain {
                if let Err(error) = handler(envelope.clone()).await {
                    record_outcome("handler_error");
                    return Err(error);
                }
            }
        }

        if matched {
            record_outcome("ok");
            return Ok(());
        }

        let result = (self.fallback)(envelope).await;
        record_outcome(if result.is_ok() {
            "fallback_ok"
        } else {
            "fallback_error"
        });
        result
    }
}

impl<E> WebhookHandler<E> for Dispatcher<E>
where
    E: 'static,
{
    type Future = BoxFuture<E>;

    fn handle(&self, envelope: Envelope) -> Self::Future {
        let dispatcher = self.clone();
        Box::pin(async move { dispatcher.dispatch(envelope).await })
    }
}

/// A builder for [`Dispatcher`].
pub struct DispatcherBuilder<E> {
    handlers: HashMap<EventKind, KindRoutes<E>>,
    fallback: Option<BoxHandler<E>>,
}

impl<E> Default for DispatcherBuilder<E> {
    fn default() -> Self {
        Self {
            handlers: HashMap::new(),
            fallback: None,
        }
    }
}

impl<E> DispatcherBuilder<E>
where
    E: 'static,
{
    /// Registers a handler for every action of an event kind.
    #[must_use]
    pub fn on<F, Fut>(self, kind: EventKind, handler: F) -> Self
    where
        F: Fn(Envelope) -> Fut + MaybeSend + MaybeSync + 'static,
        Fut: Future<Output = Result<(), E>> + MaybeSend + 'static,
    {
        self.register(kind, None, handler)
    }

    /// Registers a handler for one event kind and action pair.
    #[must_use]
    pub fn on_action<F, Fut>(self, kind: EventKind, action: Action, handler: F) -> Self
    where
        F: Fn(Envelope) -> Fut + MaybeSend + MaybeSync + 'static,
        Fut: Future<Output = Result<(), E>> + MaybeSend + 'static,
    {
        self.register(kind, Some(action), handler)
    }

    /// Replaces the default successful fallback.
    #[must_use]
    pub fn fallback<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(Envelope) -> Fut + MaybeSend + MaybeSync + 'static,
        Fut: Future<Output = Result<(), E>> + MaybeSend + 'static,
    {
        self.fallback = Some(box_handler(handler));
        self
    }

    /// Finishes the dispatcher.
    #[must_use]
    pub fn build(self) -> Dispatcher<E> {
        Dispatcher {
            handlers: Arc::new(self.handlers),
            fallback: self
                .fallback
                .unwrap_or_else(|| Arc::new(|_| Box::pin(async { Ok(()) }))),
        }
    }

    fn register<F, Fut>(mut self, kind: EventKind, action: Option<Action>, handler: F) -> Self
    where
        F: Fn(Envelope) -> Fut + MaybeSend + MaybeSync + 'static,
        Fut: Future<Output = Result<(), E>> + MaybeSend + 'static,
    {
        let routes = self.handlers.entry(kind).or_default();
        let chain = match action {
            Some(action) => routes.by_action.entry(action).or_default(),
            None => &mut routes.any_action,
        };
        chain.push(box_handler(handler));
        self
    }
}

fn box_handler<E, F, Fut>(handler: F) -> BoxHandler<E>
where
    F: Fn(Envelope) -> Fut + MaybeSend + MaybeSync + 'static,
    Fut: Future<Output = Result<(), E>> + MaybeSend + 'static,
{
    Arc::new(move |envelope| Box::pin(handler(envelope)))
}

fn record_outcome(outcome: &'static str) {
    #[cfg(feature = "tracing")]
    tracing::Span::current().record("outcome", outcome);
    let _ = outcome;
}

/// Every handler chain registered for one event kind.
struct KindRoutes<E> {
    any_action: Vec<BoxHandler<E>>,
    by_action: HashMap<Action, Vec<BoxHandler<E>>>,
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

    use bytes::Bytes;
    use tokio::sync::Mutex;

    use super::Dispatcher;
    use crate::{Action, Common, Envelope, EventKind};

    fn envelope(kind: EventKind, action: Option<Action>) -> Envelope {
        Envelope {
            delivery_id: "delivery".into(),
            kind,
            action,
            common: Common::default(),
            target_type: None,
            target_id: None,
            raw: Bytes::new(),
        }
    }

    #[tokio::test]
    async fn action_handlers_run_before_kind_handlers_in_registration_order() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let dispatcher = Dispatcher::<()>::builder()
            .on(
                EventKind::PullRequest,
                record(Arc::clone(&calls), "general-1"),
            )
            .on_action(
                EventKind::PullRequest,
                Action::Closed,
                record(Arc::clone(&calls), "specific-1"),
            )
            .on(
                EventKind::PullRequest,
                record(Arc::clone(&calls), "general-2"),
            )
            .on_action(
                EventKind::PullRequest,
                Action::Closed,
                record(Arc::clone(&calls), "specific-2"),
            )
            .build();

        dispatcher
            .dispatch(envelope(EventKind::PullRequest, Some(Action::Closed)))
            .await
            .unwrap();

        assert_eq!(
            calls.lock().await.as_slice(),
            ["specific-1", "specific-2", "general-1", "general-2"]
        );
    }

    #[tokio::test]
    async fn handlers_fail_fast() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let first_calls = Arc::clone(&calls);
        let dispatcher = Dispatcher::builder()
            .on(EventKind::Push, move |_| {
                let calls = Arc::clone(&first_calls);
                async move {
                    calls.lock().await.push("first");
                    Err("failed")
                }
            })
            .on(EventKind::Push, record(Arc::clone(&calls), "second"))
            .build();

        assert_eq!(
            dispatcher.dispatch(envelope(EventKind::Push, None)).await,
            Err("failed")
        );
        assert_eq!(calls.lock().await.as_slice(), ["first"]);
    }

    #[tokio::test]
    async fn unmatched_events_succeed_or_use_the_custom_fallback() {
        let permissive = Dispatcher::<&str>::builder().build();
        assert_eq!(
            permissive.dispatch(envelope(EventKind::Push, None)).await,
            Ok(())
        );

        let strict = Dispatcher::builder()
            .fallback(|_| async { Err("unmatched") })
            .build();
        assert_eq!(
            strict.dispatch(envelope(EventKind::Push, None)).await,
            Err("unmatched")
        );
    }

    #[tokio::test]
    async fn a_registered_kind_with_no_matching_action_still_falls_back() {
        let dispatcher = Dispatcher::builder()
            .on_action(EventKind::PullRequest, Action::Closed, |_| async { Ok(()) })
            .fallback(|_| async { Err("unmatched") })
            .build();

        assert_eq!(
            dispatcher
                .dispatch(envelope(EventKind::PullRequest, Some(Action::Opened)))
                .await,
            Err("unmatched")
        );
    }

    fn record<E>(
        calls: Arc<Mutex<Vec<&'static str>>>,
        value: &'static str,
    ) -> impl Fn(Envelope) -> Pin<Box<dyn Future<Output = Result<(), E>> + Send>> + Send + Sync + 'static
    {
        move |_| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Ok(())
            })
        }
    }
}
