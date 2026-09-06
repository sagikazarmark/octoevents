use std::{collections::HashMap, error::Error, fmt, panic::Location, sync::Arc};

use crate::{
    Action, DecodeError, Envelope, EventKind, EventMatcher, EventMeta, FromEnvelope, MaybeSend,
    MaybeSync, Payload, PayloadHandler, WebhookHandler, matcher::Slot, runtime::BoxFuture, trace,
};

// The erased handler: every flavour is registered as a function of the
// envelope, a payload handler's decode folded in. A trait object admits only
// one non-auto trait, so this cannot be written as
// `dyn Fn(..) + MaybeSend + MaybeSync` and carries the platform split by
// hand; see `runtime` for the rationale.
#[cfg(not(target_arch = "wasm32"))]
type EnvelopeFn<E> = Arc<dyn Fn(Envelope) -> BoxFuture<Result<(), E>> + Send + Sync + 'static>;
#[cfg(target_arch = "wasm32")]
type EnvelopeFn<E> = Arc<dyn Fn(Envelope) -> BoxFuture<Result<(), E>> + 'static>;

/// A handler that routes verified envelopes to other handlers by kind and
/// action.
///
/// Per delivery the dispatcher runs three tiers in the order [`Tier`] lists
/// them. The `always` chain runs first, for every delivery, and receives the
/// verified [`Envelope`], bytes included. The routed chains run next: the
/// chain for the envelope's kind and action, then the kind-wide chain. Every
/// routed handler is a [`PayloadHandler`] over some [`FromEnvelope`] input:
/// `on_payload` and `on_payload_action` route one by the kind its [`Payload`]
/// type declares, and `on` routes one over any input for the kinds and
/// actions a matcher selects. The `fallback` chain runs only if neither
/// routed chain matched, and receives the envelope as `always` does.
/// Every chain is sequential, in registration order, and stops at the first
/// error. `always` and `fallback` never count as a match, and an empty
/// fallback chain succeeds, so unmatched kinds are green in GitHub until you
/// decide otherwise. Each handler keeps its own error type; the dispatcher
/// converts them into `E` through `From` at registration.
///
/// There are no priorities and no propagation control: a handler cannot be
/// moved ahead of one registered earlier, and cannot stop the chain or
/// declare a delivery "not mine" so that a later handler takes it. The tiers
/// plus registration order cover what a webhook receiver needs, and matching
/// decided by handlers at run time would leave the route table unable to say
/// what it routes; the crate docs record the designs this was weighed
/// against under [Deliberately left out](crate#deliberately-left-out). A
/// handler that decides whether routing happens at all wraps the dispatcher
/// instead.
///
/// [`dispatch`](Self::dispatch) reports an [`Outcome`]: whether the delivery
/// was matched, and if not, whether its kind was known to the route table,
/// beside the result of the handlers that ran. The outcome is distinct from
/// success: a matched delivery can fail, and an unmatched one can succeed. A
/// delivery matches when at least one routed handler is registered for its
/// kind, or its kind and action; matching is decided by the route table,
/// never by a handler, and the `always` tier does not match. As a
/// [`WebhookHandler`] the dispatcher keeps only the result, so the receiver
/// sees an unmatched delivery as a success unless a fallback failed it. A
/// handler that wraps the dispatcher reads the outcome instead: to forward or
/// dead-letter an unmatched delivery, bytes included, without turning
/// "unhandled" into an error, or to reject a kind the route table does not
/// know while tolerating an action GitHub added to one it does.
///
/// A failure is reported as a [`DispatchError`]: the application error `E`
/// wrapped with the [`Tier`] the failing handler ran in, the delivery's ID,
/// kind and action, and the source location of the registration that put the
/// handler there. Every registration method records its caller's location,
/// so an operator reading "delivery X failed" can go to the line of code that
/// registered the handler.
///
/// The decode rule: `always` and `fallback` receive the bytes as they were
/// verified and nothing is decoded on their behalf; each routed handler
/// decodes its own input, through [`FromEnvelope`], when its route runs. So a
/// payload octocrab cannot represent still reaches `always`, every handler
/// over a consumer view or `()`, and a strict `fallback`, which answers it
/// with its own error rather than a decode error; the delivery fails only at
/// the first handler over octocrab's `WebhookEvent`, and the
/// [`DispatchError`] names that registration. A routed handler decodes only
/// when its route matches: a payload handler registered for some actions
/// decodes nothing for a delivery carrying another.
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
///     .always(|envelope: Envelope| async move {
///         println!("forward {} ({} bytes)", envelope.meta.delivery_id, envelope.raw.len());
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
///     .fallback(|envelope: Envelope| async move {
///         Err::<(), _>(AppError::Unhandled(envelope.meta.kind))
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
/// `on` routes a payload handler over any [`FromEnvelope`] input for the
/// kinds and actions a matcher selects. `()` decodes nothing, so a handler
/// over it is routed by kind and action and receives only the [`EventMeta`];
/// a consumer type implementing `FromEnvelope` itself is a view over fields
/// several kinds share. Neither needs octocrab.
///
/// ```
/// use octoevents::{Action, Dispatcher, EventKind, EventMeta};
/// # use octoevents::DecodeError;
/// # struct AppError;
/// # impl From<DecodeError> for AppError { fn from(_: DecodeError) -> Self { Self } }
/// # impl From<std::convert::Infallible> for AppError {
/// #     fn from(never: std::convert::Infallible) -> Self { match never {} }
/// # }
///
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .on((EventKind::Installation, Action::Deleted), |meta: EventMeta, (): ()| async move {
///         println!("revoke tokens for installation {:?}", meta.installation_id);
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .build();
/// # let _ = dispatcher;
/// ```
///
/// With the `octocrab` feature, octocrab's `WebhookEvent` is a `FromEnvelope`
/// too: a handler over it receives octocrab's decoded event for any kind.
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
/// A tier can continue or fail, never skip. An `always` handler that cannot
/// store an envelope fails the delivery, as it should; one that finds the
/// delivery ID already stored cannot answer the redelivery with success and
/// keep it from being routed. That policy, and what an unmatched delivery
/// means, belong in a [`WebhookHandler`] that wraps the dispatcher: it
/// persists the envelope first, answers a redelivery of a stored delivery ID
/// with success without calling `dispatch`, and reads the [`Outcome`] to
/// dead-letter or forward an unmatched delivery, bytes still in hand,
/// without a strict `fallback` turning it into an error. The dispatcher only
/// routes. The `dispatcher` example shows the wrapper.
///
/// Enabling the `octocrab` feature makes octocrab's pre-1.0 version part of
/// this crate's public API: an octocrab major bump is a breaking change for
/// handlers over its `WebhookEvent` and per-kind payload structs, not for
/// this type or its registration methods.
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

    /// Runs the `always` chain, the matching routed chains, and the fallback
    /// chain when nothing matched, in that order, and reports the
    /// [`Outcome`].
    ///
    /// The outcome carries the match the route table decided and the result
    /// of the handlers that ran: the first handler error, or the decode error
    /// of the first handler whose input could not be decoded, each wrapped in
    /// a [`DispatchError`] naming the tier it came from, the delivery, and
    /// where the handler was registered. The two are independent: a matched
    /// delivery can fail, and an unmatched one succeeds unless a fallback
    /// fails it. [`WebhookHandler::handle`] on the dispatcher keeps only the
    /// result.
    ///
    /// With the `tracing` feature, the call runs in an `octoevents.dispatch`
    /// span that records `delivery_id`, `event`, and, when the delivery has
    /// them, `action` and `installation_id`, all on open. On the way out it
    /// records `outcome` as one of four labels: `ok` (matched, every handler
    /// succeeded), `handler_error` (matched, a handler failed),
    /// `unmatched_ok` (nothing routed matched, no fallback failed) and
    /// `unmatched_error` (nothing routed matched, a handler failed, in
    /// whichever tier). On failure it also records `tier` and
    /// `registration_site`, the [`DispatchError`]'s, so the span alone says
    /// which handler failed the delivery.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            name = "octoevents.dispatch",
            skip_all,
            fields(
                delivery_id = envelope.meta.delivery_id.as_str(),
                event = envelope.meta.kind.as_str(),
                action = envelope.meta.action.as_ref().map(Action::as_str),
                installation_id = envelope.meta.installation_id,
                outcome = tracing::field::Empty,
                tier = tracing::field::Empty,
                registration_site = tracing::field::Empty,
            )
        )
    )]
    pub async fn dispatch(&self, envelope: Envelope) -> Outcome<E> {
        let (matched, routed) = self.routes.lookup(&envelope.meta);
        let result = self.run_tiers(&envelope, matched, routed).await;
        let outcome = Outcome { matched, result };
        trace::record("outcome", outcome.label());
        if let Err(error) = &outcome.result {
            trace::record("tier", error.tier.as_str());
            trace::record_display("registration_site", error.registration_site);
        }
        outcome
    }

    /// Runs the `always` chain, then either the routed chains or the fallback
    /// chain, stopping at the first error.
    async fn run_tiers(
        &self,
        envelope: &Envelope,
        matched: Match,
        routed: impl Iterator<Item = &[Route<E>]>,
    ) -> Result<(), DispatchError<E>> {
        run_chain(envelope, Tier::Always, &self.routes.always).await?;

        match matched {
            Match::Matched => {
                for chain in routed {
                    run_chain(envelope, Tier::Route, chain).await?;
                }
                Ok(())
            }
            Match::UnmatchedAction | Match::UnmatchedKind => {
                run_chain(envelope, Tier::Fallback, &self.routes.fallback).await
            }
        }
    }
}

/// Runs one chain in order, stopping at the first error and wrapping it with
/// the tier, the delivery, and where the failing route was registered. The
/// clones for the error happen only on that path.
async fn run_chain<E>(
    envelope: &Envelope,
    tier: Tier,
    chain: &[Route<E>],
) -> Result<(), DispatchError<E>> {
    for route in chain {
        if let Err(source) = (route.handler)(envelope.clone()).await {
            let meta = &envelope.meta;
            return Err(DispatchError {
                tier,
                registration_site: route.registration_site,
                delivery_id: meta.delivery_id.clone(),
                kind: meta.kind.clone(),
                action: meta.action.clone(),
                source,
            });
        }
    }
    Ok(())
}

impl<E> WebhookHandler for Dispatcher<E>
where
    E: From<DecodeError> + 'static,
{
    type Error = DispatchError<E>;

    /// Dispatches the envelope and keeps only the result: an unmatched
    /// delivery succeeds unless a fallback fails it. The `octoevents.dispatch`
    /// span records the outcome on this path too.
    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        self.dispatch(envelope).await.result
    }
}

/// What one dispatch reports: whether the delivery matched, and whether the
/// handlers that ran succeeded.
///
/// The two are independent. `matched` is decided by the route table alone,
/// never by a handler, so it is known even when the `always` tier failed
/// before routing began. `result` is `Ok` when every handler that ran
/// succeeded, and otherwise the first error, whichever tier it came from,
/// wrapped in a [`DispatchError`] that names the tier, the delivery, and
/// where the failing handler was registered. A matched delivery can fail; an
/// unmatched one succeeds unless a fallback fails it.
///
/// A handler wrapping a [`Dispatcher`] reads both to set policy the tiers
/// cannot: forward or dead-letter an unmatched delivery, bytes included,
/// without turning "unhandled" into an error, or reject a kind the route
/// table does not know while tolerating an action GitHub added to a kind it
/// does. The receiver never sees this type: [`WebhookHandler::handle`] on the
/// dispatcher returns `result` alone.
///
/// With the `tracing` feature, the `octoevents.dispatch` span records the
/// outcome as one label from the same two axes, so a dashboard filters on
/// what a policy matches on:
///
/// | `matched`                            | `result` | `outcome`         |
/// |--------------------------------------|----------|-------------------|
/// | `Matched`                            | `Ok`     | `ok`              |
/// | `Matched`                            | `Err`    | `handler_error`   |
/// | `UnmatchedAction` or `UnmatchedKind` | `Ok`     | `unmatched_ok`    |
/// | `UnmatchedAction` or `UnmatchedKind` | `Err`    | `unmatched_error` |
///
/// The label says whether the delivery matched and whether it failed, not
/// which tier failed it: an `always` handler failing an unrouted kind is
/// `unmatched_error` with no fallback registered. The tier and the
/// registration site are fields of their own on the same span.
///
/// ```
/// use octoevents::{DispatchError, Dispatcher, Envelope, Match, WebhookHandler};
/// # use octoevents::DecodeError;
/// # #[derive(Debug)]
/// # struct AppError;
/// # impl From<DecodeError> for AppError { fn from(_: DecodeError) -> Self { Self } }
///
/// /// Dead-letters deliveries of kinds the dispatcher never registered.
/// struct DeadLetter {
///     dispatcher: Dispatcher<AppError>,
/// }
///
/// impl WebhookHandler for DeadLetter {
///     // The dispatcher's error passes through, tier and registration site included.
///     type Error = DispatchError<AppError>;
///
///     async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
///         // The clone shares the bytes; the wrapper still holds them.
///         let outcome = self.dispatcher.dispatch(envelope.clone()).await;
///         match outcome.matched {
///             // Routed, or an action GitHub added to a kind this app handles.
///             Match::Matched | Match::UnmatchedAction => outcome.result,
///             Match::UnmatchedKind => {
///                 outcome.result?;
///                 println!("dead-letter {} ({} bytes)", envelope.meta.delivery_id, envelope.raw.len());
///                 Ok(())
///             }
///         }
///     }
/// }
/// # let _ = DeadLetter { dispatcher: Dispatcher::<AppError>::builder().build() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "an outcome carries the handlers' result in its `result` field"]
pub struct Outcome<E> {
    /// Whether the route table matched the delivery, and if not, whether it
    /// knew the kind.
    pub matched: Match,
    /// `Ok` when every handler that ran succeeded; otherwise the first error,
    /// with the tier and registration site it came from.
    pub result: Result<(), DispatchError<E>>,
}

impl<E> Outcome<E> {
    /// The value the `octoevents.dispatch` span records as `outcome`.
    fn label(&self) -> &'static str {
        match (self.matched, self.result.is_ok()) {
            (Match::Matched, true) => "ok",
            (Match::Matched, false) => "handler_error",
            (Match::UnmatchedAction | Match::UnmatchedKind, true) => "unmatched_ok",
            (Match::UnmatchedAction | Match::UnmatchedKind, false) => "unmatched_error",
        }
    }
}

/// Whether a delivery matched the route table.
///
/// A delivery matches when at least one routed handler is registered for its
/// kind, or for its kind and action. The `always` and `fallback` tiers never
/// count: a delivery handled only by them is unmatched. When nothing
/// matched, the route table still says whether it knows the kind, so a
/// strict policy can reject a kind it never registered while tolerating an
/// action GitHub added to one it did.
///
/// The three cases are exhaustive by construction of the route table, which
/// is keyed by kind and then by action, so a policy matches on them without
/// a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Match {
    /// At least one routed handler is registered for the delivery's kind, or
    /// for its kind and action.
    Matched,
    /// Routed handlers are registered for the delivery's kind, but none for
    /// its action (or for a delivery without one).
    UnmatchedAction,
    /// No routed handler is registered for the delivery's kind.
    UnmatchedKind,
}

/// The error of a failed dispatch: the application error, and where in the
/// dispatch and in the consumer's source it came from.
///
/// The dispatcher wraps the error of the handler that failed the delivery
/// with what it knew and the handler did not: the [`Tier`] the handler ran
/// in, the delivery's ID, kind and action, and the source location of the
/// registration (`always`, `on`, `on_payload`, `on_payload_action` or
/// `fallback`) that put the handler there. Every registration method records
/// its caller's location at compile time, so the cost is one static
/// reference per registration, on `wasm32` as anywhere. A decode failure is
/// reported at the handler that needed the decode: its tier, its
/// registration site, and `E::from` of the [`DecodeError`].
///
/// [`Display`](fmt::Display) names where, not why: the tier, the delivery,
/// and the registration site. Why is the [`source`](Error::source), the
/// application error, so a reporter that walks the chain prints both, and
/// [`into_source`](Self::into_source) drops the wrapping for code that wants
/// the application error alone. The [`Error`] impl asks `Error + 'static` of
/// `E`, what any source in a chain must be; for an `E` that is not one,
/// `Box<dyn Error + Send + Sync>` included, the dispatcher still builds, the
/// error still displays, and `into_source` returns the boxed error, which is
/// one.
///
/// A wrapping handler that passes the dispatcher's result through keeps the
/// tier and registration site by making this its error type; the receiver
/// accepts it as it does any error, and hands it to the observer registered
/// with `WebhookReceiverBuilder::on_error` before answering 500.
///
/// The dispatcher produces this and consumers only read it, so it is
/// `#[non_exhaustive]`: another field (the handler's type name, say) can be
/// added without that becoming a breaking change here. A test that needs one
/// dispatches to a handler that fails.
///
/// ```text
/// delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (pull_request.opened) failed in the route tier at the handler registered at src/main.rs:42:10
///   caused by: database is down
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DispatchError<E> {
    /// The tier the failing handler ran in.
    pub tier: Tier,
    /// Where the failing handler was registered: the call to the registration
    /// method in the consumer's source.
    pub registration_site: &'static Location<'static>,
    /// The `X-GitHub-Delivery` value of the delivery that failed.
    pub delivery_id: String,
    /// The kind of the delivery that failed.
    pub kind: EventKind,
    /// The action of the delivery that failed, when it had one.
    pub action: Option<Action>,
    /// The application error: the handler's own, converted through `From`, or
    /// `E::from` of the [`DecodeError`] when the handler's input could not be
    /// decoded.
    pub source: E,
}

impl<E> DispatchError<E> {
    /// Drops the wrapping and returns the application error.
    ///
    /// The one-call path from a dispatch result to the application error, for
    /// code that reports the delivery and the handler by other means or an
    /// `E` that is not an [`Error`] itself.
    #[must_use]
    pub fn into_source(self) -> E {
        self.source
    }
}

// Written out rather than derived through thiserror: the action is optional
// and joins the kind with a dot only when present (`pull_request.opened`,
// `ping`), which a format string cannot express, and the `Error` impl must
// stay separate so `Display` holds for an `E` that is not an `Error`.
impl<E> fmt::Display for DispatchError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "delivery {} ({}", self.delivery_id, self.kind)?;
        if let Some(action) = &self.action {
            write!(formatter, ".{action}")?;
        }
        write!(
            formatter,
            ") failed in the {} tier at the handler registered at {}",
            self.tier, self.registration_site
        )
    }
}

impl<E> Error for DispatchError<E>
where
    E: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// The tiers a [`Dispatcher`] runs a delivery through, in order.
///
/// Named by a [`DispatchError`] to say which one the failing handler ran in.
/// The three are the dispatcher's definition, so a policy matches on them
/// without a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// The `always` chain: webhook handlers over the envelope, bytes
    /// included, that run for every delivery before routing.
    Always,
    /// The routed chains: the payload handlers `on`, `on_payload` and
    /// `on_payload_action` registered for the delivery's kind and action.
    Route,
    /// The `fallback` chain: webhook handlers that run only when no routed
    /// handler matched.
    Fallback,
}

impl Tier {
    /// The tier's name as it appears in a [`DispatchError`]'s message.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Route => "route",
            Self::Fallback => "fallback",
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
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
    /// Registers a webhook handler that runs for every delivery, before
    /// routing.
    ///
    /// It receives the verified [`Envelope`], bytes included, and nothing is
    /// decoded on its behalf, so it runs even for a payload no typed handler
    /// can decode: the tier for audit, metrics, and forwarding. Its failure
    /// fails the delivery, and it never counts as a match, so a strict
    /// fallback still rejects kinds nothing else handles. It cannot skip: a
    /// handler that decides whether a delivery is routed at all (to answer a
    /// redelivery of a stored delivery ID with success, say) wraps the
    /// dispatcher instead.
    ///
    /// Like every registration method, this records where it was called so a
    /// [`DispatchError`] can point back at the registration.
    #[must_use]
    #[track_caller]
    pub fn always<H>(mut self, handler: H) -> Self
    where
        H: WebhookHandler + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.routes
            .always
            .push(Route::registered(webhook_handler(handler)));
        self
    }

    /// Registers a payload handler for the kinds and actions the matcher
    /// selects.
    ///
    /// The handler's input is any [`FromEnvelope`], whose docs list the
    /// shipped impls. A [`Payload`] registered here decodes with its kind
    /// check, so a matcher that disagrees with the kind the view declares
    /// fails the delivery with [`DecodeError::KindMismatch`]; `()` decodes
    /// nothing, for a handler that needs only the [`EventMeta`]. Each route
    /// decodes its own input when it runs, and a decode failure fails the
    /// delivery at that position: the [`DispatchError`] names this
    /// registration.
    ///
    /// A handler registered under several slots is shared, not duplicated.
    /// `P` is inferred from a closure's parameter type or from a struct that
    /// implements [`PayloadHandler`] for one input; a struct that implements
    /// it for several names the input: `on::<Sender, _>(matcher, auditor)`.
    #[must_use]
    #[track_caller]
    pub fn on<P, H>(mut self, matcher: impl Into<EventMatcher>, handler: H) -> Self
    where
        P: FromEnvelope + 'static,
        H: PayloadHandler<P> + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        let route = Route::registered(payload_handler(handler));
        self.insert_each(matcher.into().into_slots(), &route);
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
    #[track_caller]
    pub fn on_payload<P, H>(mut self, handler: H) -> Self
    where
        P: Payload + 'static,
        H: PayloadHandler<P> + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        let route = Route::registered(payload_handler(handler));
        self.insert(Slot::any_action(P::KIND), route);
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
    #[track_caller]
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
        let route = Route::registered(payload_handler(handler));
        let slots = actions
            .into_iter()
            .map(|action| Slot::action(P::KIND, action));
        self.insert_each(slots, &route);
        self
    }

    /// Appends a webhook handler to the chain that runs when no routed chain
    /// matched.
    ///
    /// Several may be registered; they run in order and stop at the first
    /// error. "Log it, then reject it" is two small handlers. Like `always`,
    /// the chain receives the [`Envelope`] and nothing is decoded on its
    /// behalf, so a strict fallback reports its own error for an unmatched
    /// payload nothing can decode, not a decode error. A fallback can only
    /// continue or fail: to forward or dead-letter an unmatched delivery
    /// without failing it, a handler wrapping the dispatcher reads the
    /// [`Outcome`] instead.
    #[must_use]
    #[track_caller]
    pub fn fallback<H>(mut self, handler: H) -> Self
    where
        H: WebhookHandler + MaybeSend + MaybeSync + 'static,
        E: From<H::Error>,
    {
        self.routes
            .fallback
            .push(Route::registered(webhook_handler(handler)));
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

fn webhook_handler<E, H>(handler: H) -> EnvelopeFn<E>
where
    E: From<H::Error> + 'static,
    H: WebhookHandler + MaybeSend + MaybeSync + 'static,
{
    let handler = Arc::new(handler);
    Arc::new(move |envelope: Envelope| {
        let handler = Arc::clone(&handler);
        Box::pin(async move { handler.handle(envelope).await.map_err(E::from) })
    })
}

/// Erases a payload handler behind its decode: the route decodes `P` from
/// the envelope when it runs, so a route that never matches never decodes,
/// and a decode failure is this route's failure.
fn payload_handler<E, P, H>(handler: H) -> EnvelopeFn<E>
where
    E: From<DecodeError> + From<H::Error> + 'static,
    P: FromEnvelope + 'static,
    H: PayloadHandler<P> + MaybeSend + MaybeSync + 'static,
{
    let handler = Arc::new(handler);
    Arc::new(move |envelope: Envelope| {
        let handler = Arc::clone(&handler);
        Box::pin(async move {
            let payload = P::from_envelope(&envelope).map_err(E::from)?;
            handler
                .handle(envelope.meta, payload)
                .await
                .map_err(E::from)
        })
    })
}

/// One registered handler and where it was registered.
///
/// A handler registered under several slots is one route cloned per slot:
/// the erased handler is shared, and every clone points at the same
/// registration.
struct Route<E> {
    handler: EnvelopeFn<E>,
    /// The call to the registration method, captured through
    /// `#[track_caller]`: a static reference, on `wasm32` as anywhere.
    registration_site: &'static Location<'static>,
}

impl<E> Route<E> {
    /// Pairs an erased handler with the location of the registration method
    /// that is being called. `#[track_caller]` here and on that method makes
    /// the location the consumer's, two frames up, not this one.
    #[track_caller]
    fn registered(handler: EnvelopeFn<E>) -> Self {
        Self {
            handler,
            registration_site: Location::caller(),
        }
    }
}

impl<E> Clone for Route<E> {
    fn clone(&self) -> Self {
        Self {
            handler: Arc::clone(&self.handler),
            registration_site: self.registration_site,
        }
    }
}

// The erased handler is never `Debug`; where it was registered is what an
// operator reading the route table wants, so a route prints as
// `Route(src/main.rs:12:10, ..)`, the `..` standing for the elided handler
// as in `WebhookReceiver`'s `Debug`. No bound on `E`: the dispatcher is
// `Debug` for any error type, as it is `Clone` for any.
impl<E> fmt::Debug for Route<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Route")
            .field(&format_args!("{}", self.registration_site))
            .finish_non_exhaustive()
    }
}

/// Every chain a dispatcher can run.
struct Routes<E> {
    always: Vec<Route<E>>,
    by_kind: HashMap<EventKind, KindRoutes<E>>,
    fallback: Vec<Route<E>>,
}

impl<E> Routes<E> {
    /// Looks one delivery up in the route table: the match it decides and the
    /// routed chains it selects, the action-specific chain before the
    /// kind-wide one.
    ///
    /// This is the whole of matching: the tiers that run afterwards cannot
    /// change it. Routes are keyed by kind first so the lookup is entirely by
    /// reference: no `EventKind` or `Action` is cloned to build a key.
    fn lookup(&self, meta: &EventMeta) -> (Match, impl Iterator<Item = &[Route<E>]>) {
        let kind_routes = self.by_kind.get(&meta.kind);
        let specific = kind_routes.and_then(|routes| {
            meta.action
                .as_ref()
                .and_then(|action| routes.by_action.get(action))
        });
        // A kind registered only under some actions has an empty kind-wide
        // chain, which must not count as a match.
        let any_action = kind_routes
            .map(|routes| &routes.any_action)
            .filter(|chain| !chain.is_empty());

        let matched = match (kind_routes, specific.or(any_action)) {
            (_, Some(_)) => Match::Matched,
            (Some(_), None) => Match::UnmatchedAction,
            (None, None) => Match::UnmatchedKind,
        };
        let chains = specific.into_iter().chain(any_action).map(Vec::as_slice);
        (matched, chains)
    }

    /// Prints the route table under the name of the type that owns it, so
    /// the dispatcher and its builder read alike.
    fn fmt_as(&self, name: &str, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(name)
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

    use super::{DispatchError, Dispatcher, Match, Outcome, Tier};
    use crate::{
        Action, DecodeError, Envelope, EventKind, EventMatcher, EventMeta,
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

    /// The handlers' result with the dispatch error unwrapped to its source,
    /// for tests that check which handler failed rather than where it was
    /// registered.
    fn unwrapped<E>(outcome: Outcome<E>) -> Result<(), E> {
        outcome.result.map_err(DispatchError::into_source)
    }

    /// [`unwrapped`] keeping the match beside the result.
    fn unwrapped_outcome<E>(outcome: Outcome<E>) -> (Match, Result<(), E>) {
        (
            outcome.matched,
            outcome.result.map_err(DispatchError::into_source),
        )
    }

    /// A webhook handler that appends `value` to the shared log.
    fn record(
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
    fn fail(
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

    /// A payload handler over [`AnyPullRequest`] that appends `value` to the
    /// shared log and then fails.
    fn fail_payload(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(EventMeta, AnyPullRequest) -> Recorded<&'static str> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_, _| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Err(value)
            })
        }
    }

    /// A payload handler over `()` that appends `value` to the shared log:
    /// routed by kind and action, with nothing decoded.
    fn record_meta(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(EventMeta, ()) -> Recorded<AppError> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_, ()| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Ok(())
            })
        }
    }

    /// A payload handler over `()` that appends `value` to the shared log
    /// and then fails.
    fn fail_meta(
        calls: &Calls,
        value: &'static str,
    ) -> impl Fn(EventMeta, ()) -> Recorded<&'static str> + Send + Sync + 'static {
        let calls = Arc::clone(calls);
        move |_, ()| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push(value);
                Err(value)
            })
        }
    }

    /// A payload handler over octocrab's `WebhookEvent` that appends `value`
    /// to the shared log.
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

    #[tokio::test]
    async fn tiers_run_always_then_action_then_kind_in_registration_order() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(EventKind::PullRequest, record_meta(&calls, "kind-1"))
            .always(record(&calls, "always-1"))
            .on(
                (EventKind::PullRequest, Action::Opened),
                record_meta(&calls, "action-1"),
            )
            .on(EventKind::PullRequest, record_meta(&calls, "kind-2"))
            .always(record(&calls, "always-2"))
            .on(
                (EventKind::PullRequest, Action::Opened),
                record_meta(&calls, "action-2"),
            )
            .fallback(record(&calls, "fallback"))
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();

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

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();

        assert_eq!(
            calls.lock().await.as_slice(),
            ["action-1", "action-2", "kind-1", "kind-2"]
        );
    }

    #[tokio::test]
    async fn tiers_run_always_then_routes_then_fallback_in_registration_order() {
        // Registration order is interleaved across tiers on purpose: the tier
        // decides when a handler runs, and only order within a tier follows
        // registration.
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(record_payload(&calls, "route-1"))
            .always(record(&calls, "always-1"))
            .fallback(record(&calls, "fallback-1"))
            .on_payload(record_payload(&calls, "route-2"))
            .always(record(&calls, "always-2"))
            .fallback(record(&calls, "fallback-2"))
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(
            calls.lock().await.as_slice(),
            ["always-1", "always-2", "route-1", "route-2"]
        );

        calls.lock().await.clear();
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
        assert_eq!(
            calls.lock().await.as_slice(),
            ["always-1", "always-2", "fallback-1", "fallback-2"]
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
            unwrapped(dispatcher.dispatch(installation_created()).await),
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["audit", "unmatched"]);
    }

    #[tokio::test]
    async fn always_runs_for_a_payload_octocrab_cannot_represent() {
        let calls = Calls::default();
        let handler_calls = Arc::clone(&calls);
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(move |envelope: Envelope| {
                let calls = Arc::clone(&handler_calls);
                async move {
                    calls.lock().await.push("audit");
                    assert_eq!(envelope.meta.kind, EventKind::PullRequest);
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .build();

        // Nothing is decoded on the always tier's behalf, so the delivery
        // succeeds although octocrab cannot represent it.
        assert_eq!(dispatcher.dispatch(unrepresentable()).await.result, Ok(()));
        assert_eq!(calls.lock().await.as_slice(), ["audit"]);
    }

    #[tokio::test]
    async fn always_and_fallback_receive_the_envelope_with_its_bytes() {
        type Seen = Arc<Mutex<Vec<(&'static str, EventKind, bytes::Bytes)>>>;

        fn forward(
            seen: &Seen,
            tier: &'static str,
        ) -> impl Fn(Envelope) -> Recorded<AppError> + Send + Sync + 'static {
            let seen = Arc::clone(seen);
            move |envelope| {
                let seen = Arc::clone(&seen);
                Box::pin(async move {
                    seen.lock()
                        .await
                        .push((tier, envelope.meta.kind, envelope.raw));
                    Ok(())
                })
            }
        }

        // A forwarder in either tier sees the exact bytes the envelope
        // carries, not a metadata-only view of it.
        let seen = Seen::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(forward(&seen, "always"))
            .fallback(forward(&seen, "fallback"))
            .build();

        let envelope = check_run_completed();
        dispatcher.dispatch(envelope.clone()).await.result.unwrap();

        assert_eq!(
            seen.lock().await.as_slice(),
            [
                ("always", EventKind::CheckRun, envelope.raw.clone()),
                ("fallback", EventKind::CheckRun, envelope.raw),
            ]
        );
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
            unwrapped(dispatcher.dispatch(unrepresentable()).await),
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
            unwrapped(dispatcher.dispatch(check_run_completed()).await),
            Err(AppError::Handler("reject"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["log", "reject"]);

        calls.lock().await.clear();
        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["pull-request"]);
    }

    #[tokio::test]
    async fn unmatched_deliveries_succeed_when_the_fallback_chain_is_empty() {
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(|_: EventMeta, _: AnyPullRequest| async { Ok::<_, AppError>(()) })
            .build();

        assert_eq!(dispatcher.dispatch(unknown()).await.result, Ok(()));
        assert_eq!(dispatcher.dispatch(ping()).await.result, Ok(()));
        assert_eq!(
            dispatcher.dispatch(check_run_completed()).await.result,
            Ok(())
        );
    }

    #[tokio::test]
    async fn a_matched_failure_is_distinguishable_from_an_unmatched_fallback_failure() {
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(|_: EventMeta, _: AnyPullRequest| async { Err::<(), _>("routed") })
            .fallback(|_: Envelope| async { Err::<(), _>("unmatched") })
            .build();

        // Both deliveries fail. The result alone cannot say whether a routed
        // handler or a strict fallback failed them; the match can.
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(pull_request_opened()).await),
            (Match::Matched, Err(AppError::Handler("routed")))
        );
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(check_run_completed()).await),
            (Match::UnmatchedKind, Err(AppError::Handler("unmatched")))
        );
    }

    #[tokio::test]
    async fn an_unmatched_outcome_says_whether_the_route_table_knows_the_kind() {
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload_action([Action::Opened], |_: EventMeta, _: AnyPullRequest| async {
                Ok::<_, AppError>(())
            })
            .build();

        assert_eq!(
            dispatcher.dispatch(pull_request_opened()).await,
            Outcome {
                matched: Match::Matched,
                result: Ok(()),
            }
        );

        // Routes exist for `pull_request`, none for `closed`: the kind is
        // known, so a strict policy can tolerate an action it did not
        // register. The same holds for a delivery of the kind carrying no
        // action at all.
        assert_eq!(
            dispatcher.dispatch(pull_request(Action::Closed)).await,
            Outcome {
                matched: Match::UnmatchedAction,
                result: Ok(()),
            }
        );
        assert_eq!(
            dispatcher.dispatch(unrepresentable()).await,
            Outcome {
                matched: Match::UnmatchedAction,
                result: Ok(()),
            }
        );

        // No route mentions `check_run`, nor a kind this crate does not know.
        assert_eq!(
            dispatcher.dispatch(check_run_completed()).await,
            Outcome {
                matched: Match::UnmatchedKind,
                result: Ok(()),
            }
        );
        assert_eq!(
            dispatcher.dispatch(unknown()).await,
            Outcome {
                matched: Match::UnmatchedKind,
                result: Ok(()),
            }
        );
    }

    #[tokio::test]
    async fn the_match_is_decided_by_the_route_table_whichever_tier_fails() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(fail(&calls, "audit"))
            .on_payload(record_payload(&calls, "routed"))
            .build();

        // The always tier fails before routing begins. The route table still
        // says which delivery would have been routed and which would not.
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(pull_request_opened()).await),
            (Match::Matched, Err(AppError::Handler("audit")))
        );
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(check_run_completed()).await),
            (Match::UnmatchedKind, Err(AppError::Handler("audit")))
        );
        assert_eq!(calls.lock().await.as_slice(), ["audit", "audit"]);
    }

    #[tokio::test]
    async fn handle_keeps_the_result_and_drops_the_match() {
        use crate::WebhookHandler as _;

        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(|_: EventMeta, _: AnyPullRequest| async { Err::<(), _>("routed") })
            .build();

        // What the receiver sees: an unmatched delivery succeeds, a matched
        // one reports its handler's error, and neither says which it was.
        assert_eq!(
            dispatcher
                .handle(check_run_completed())
                .await
                .map_err(DispatchError::into_source),
            Ok(())
        );
        assert_eq!(
            dispatcher
                .handle(pull_request_opened())
                .await
                .map_err(DispatchError::into_source),
            Err(AppError::Handler("routed"))
        );
    }

    #[tokio::test]
    async fn a_failure_names_the_tier_the_delivery_and_the_registration_site() {
        // The location is that of the registration method's name, so the
        // failing handler is registered on the line after `line!()`.
        let calls = Calls::default();
        let builder = Dispatcher::<AppError>::builder();
        let registration_site = line!() + 1;
        let dispatcher = builder.on_payload(fail_payload(&calls, "routed")).build();

        let error = dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap_err();

        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.delivery_id, "delivery");
        assert_eq!(error.kind, EventKind::PullRequest);
        assert_eq!(error.action, Some(Action::Opened));
        assert_eq!(error.registration_site.file(), file!());
        assert_eq!(error.registration_site.line(), registration_site);
        assert_eq!(error.source, AppError::Handler("routed"));
    }

    #[tokio::test]
    async fn every_registration_method_names_its_tier_and_its_own_line() {
        let calls = Calls::default();

        /// Registers a failing handler with the named method and pairs the
        /// builder with the line of the call, so each case pins its own
        /// registration site however the call is formatted.
        macro_rules! registered {
            ($method:ident($($argument:expr),*)) => {
                (Dispatcher::<AppError>::builder().$method($($argument),*), line!())
            };
        }

        // One failing handler per registration method, so the locations are
        // distinct and each error must carry its own. Registration order is
        // irrelevant to the tier: the method decides it.
        let cases = [
            (
                registered!(always(fail(&calls, "always"))),
                Tier::Always,
                "always",
            ),
            (
                registered!(on(EventKind::PullRequest, fail_meta(&calls, "on"))),
                Tier::Route,
                "on",
            ),
            (
                registered!(on_payload_action(
                    [Action::Opened],
                    fail_payload(&calls, "action")
                )),
                Tier::Route,
                "action",
            ),
            (
                registered!(on_payload(fail_payload(&calls, "kind"))),
                Tier::Route,
                "kind",
            ),
            (
                registered!(fallback(fail(&calls, "fallback"))),
                Tier::Fallback,
                "fallback",
            ),
        ];
        for ((builder, line), tier, value) in cases {
            let error = builder
                .build()
                .dispatch(pull_request_opened())
                .await
                .result
                .unwrap_err();
            assert_eq!(error.tier, tier, "{value}");
            assert_eq!(error.registration_site.line(), line, "{value}");
            assert_eq!(error.source, AppError::Handler(value));
        }
    }

    #[tokio::test]
    async fn the_fallback_tier_reports_a_delivery_without_an_action() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .fallback(fail(&calls, "unmatched"))
            .build();

        let error = dispatcher.dispatch(ping()).await.result.unwrap_err();

        assert_eq!(error.tier, Tier::Fallback);
        assert_eq!(error.kind, EventKind::Ping);
        assert_eq!(error.action, None);
    }

    #[tokio::test]
    async fn a_decode_failure_is_reported_at_the_handler_that_needed_the_decode() {
        // A view the payload below cannot satisfy.
        #[derive(serde::Deserialize)]
        struct Number {
            #[allow(
                dead_code,
                reason = "the field is required so the decode fails; nothing reads it"
            )]
            number: u64,
        }
        crate::impl_payload!(Number => EventKind::PullRequest);

        fn needs_number(_: EventMeta, _: Number) -> Recorded<AppError> {
            Box::pin(async { Ok(()) })
        }

        // The handlers before it never decode, so the failure is attributed to
        // the payload handler's own registration, not the first one.
        let calls = Calls::default();
        let builder = Dispatcher::<AppError>::builder().always(record(&calls, "always"));
        let registration_site = line!() + 1;
        let dispatcher = builder.on_payload(needs_number).build();

        let error = dispatcher
            .dispatch(envelope_with_action(
                EventKind::PullRequest,
                Action::Opened,
                br#"{"action":"opened"}"#,
            ))
            .await
            .result
            .unwrap_err();

        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.registration_site.line(), registration_site);
        assert_eq!(error.source, AppError::Decode);
        assert_eq!(calls.lock().await.as_slice(), ["always"]);
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn an_event_decode_failure_is_reported_at_the_handler_over_webhook_event_that_needed_it()
    {
        // The payload route before the handler over `WebhookEvent` decodes
        // its own view and succeeds; octocrab's decode fails at that handler,
        // and the error names its registration.
        let calls = Calls::default();
        let builder = Dispatcher::<AppError>::builder().on_payload(record_payload(&calls, "view"));
        let registration_site = line!() + 1;
        let dispatcher = builder.on(EventKind::PullRequest, record_event(&calls, "event"));

        let error = dispatcher
            .build()
            .dispatch(unrepresentable())
            .await
            .result
            .unwrap_err();

        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.registration_site.line(), registration_site);
        assert_eq!(error.source, AppError::Decode);
        assert_eq!(calls.lock().await.as_slice(), ["view"]);
    }

    #[tokio::test]
    async fn display_names_the_tier_and_the_registration_site_and_source_yields_the_application_error()
     {
        use std::error::Error as _;

        #[derive(Debug, thiserror::Error)]
        enum ServiceError {
            #[error(transparent)]
            Decode(#[from] DecodeError),
            #[error("database is down")]
            Database,
        }

        fn database_down(_: EventMeta, _: AnyPullRequest) -> Recorded<ServiceError> {
            Box::pin(async { Err(ServiceError::Database) })
        }

        let builder = Dispatcher::<ServiceError>::builder();
        let registration_site = line!() + 1;
        let dispatcher = builder.on_payload(database_down).build();

        let error = dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap_err();

        // The message names where, not why: the source chain says why, as
        // it does for every other error in the crate.
        assert_eq!(error.registration_site.line(), registration_site);
        assert_eq!(
            error.to_string(),
            format!(
                "delivery delivery (pull_request.opened) failed in the route tier at the handler \
                 registered at {}",
                error.registration_site
            )
        );
        let source = error.source().expect("the application error is the source");
        assert_eq!(source.to_string(), "database is down");
        assert!(matches!(
            source.downcast_ref::<ServiceError>(),
            Some(ServiceError::Database)
        ));

        // Without an action the parenthesised part is the kind alone.
        let dispatcher = Dispatcher::<ServiceError>::builder()
            .fallback(|_: Envelope| async { Err::<(), _>(ServiceError::Database) })
            .build();
        let error = dispatcher.dispatch(ping()).await.result.unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("delivery delivery (ping) failed in the fallback tier at the handler"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_boxed_dyn_error_application_error_still_works() {
        // `DispatchError<Box<dyn Error + Send + Sync>>` is not itself an
        // `Error`, as `Box<dyn Error + Send + Sync>` is not, but the
        // dispatcher builds, the error displays, and `into_source` returns
        // the boxed error, which is one.
        type Boxed = Box<dyn std::error::Error + Send + Sync>;

        let dispatcher = Dispatcher::<Boxed>::builder()
            .on_payload(|_: EventMeta, _: AnyPullRequest| async { Err::<(), Boxed>("boom".into()) })
            .build();

        let error = dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap_err();

        assert!(error.to_string().contains("route tier"), "{error}");
        assert_eq!(error.into_source().to_string(), "boom");
    }

    #[tokio::test]
    async fn a_wrapper_forwards_unmatched_deliveries_with_their_bytes() {
        use crate::WebhookHandler;

        /// Sets the policy the tiers cannot: an action GitHub added to a kind
        /// the dispatcher handles is tolerated, and a delivery of a kind it
        /// never registered is dead-lettered, bytes included, instead of
        /// being turned into an error.
        struct DeadLetter {
            dispatcher: Dispatcher<AppError>,
            letters: Arc<Mutex<Vec<Envelope>>>,
        }

        impl WebhookHandler for DeadLetter {
            // The dispatcher's error passes through, tier and registration site included.
            type Error = DispatchError<AppError>;

            async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
                // The dispatcher takes the envelope by value; the clone shares
                // the bytes, so the wrapper still holds them afterwards.
                let outcome = self.dispatcher.dispatch(envelope.clone()).await;
                match outcome.matched {
                    Match::Matched | Match::UnmatchedAction => outcome.result,
                    Match::UnmatchedKind => {
                        outcome.result?;
                        self.letters.lock().await.push(envelope);
                        Ok(())
                    }
                }
            }
        }

        let calls = Calls::default();
        let letters = Arc::new(Mutex::new(Vec::new()));
        let wrapper = DeadLetter {
            dispatcher: Dispatcher::<AppError>::builder()
                .always(record(&calls, "audit"))
                .on_payload_action([Action::Opened], record_payload(&calls, "triage"))
                .build(),
            letters: Arc::clone(&letters),
        };

        // Matched, and known kind with an unregistered action: nothing is
        // dead-lettered and both succeed.
        wrapper.handle(pull_request_opened()).await.unwrap();
        wrapper.handle(pull_request(Action::Closed)).await.unwrap();
        assert!(letters.lock().await.is_empty());

        // Unknown kind: dead-lettered as the exact envelope the dispatcher
        // saw, and still a success towards GitHub.
        let envelope = check_run_completed();
        wrapper.handle(envelope.clone()).await.unwrap();
        assert_eq!(letters.lock().await.as_slice(), [envelope]);
        assert_eq!(
            calls.lock().await.as_slice(),
            ["audit", "triage", "audit", "audit"]
        );
    }

    #[tokio::test]
    async fn a_registered_kind_with_no_matching_action_still_falls_back() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                (EventKind::PullRequest, Action::Closed),
                record_meta(&calls, "closed"),
            )
            .fallback(fail(&calls, "unmatched"))
            .build();

        assert_eq!(
            unwrapped(dispatcher.dispatch(pull_request_opened()).await),
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

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["triage"]);

        calls.lock().await.clear();
        dispatcher
            .dispatch(pull_request(Action::Reopened))
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["triage"]);

        // `closed` was never registered, so the kind alone earns no match:
        // the strict fallback rejects it instead of the handler silently
        // widening to every pull-request action.
        calls.lock().await.clear();
        assert_eq!(
            unwrapped(dispatcher.dispatch(pull_request(Action::Closed)).await),
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["unmatched"]);
    }

    #[tokio::test]
    async fn on_routes_a_handler_over_unit_by_kind_and_action_with_nothing_decoded() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                (EventKind::Installation, Action::Deleted),
                move |meta: EventMeta, (): ()| {
                    let seen = Arc::clone(&handler_seen);
                    async move {
                        seen.lock()
                            .await
                            .push((meta.delivery_id, meta.installation_id));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .fallback(|_: Envelope| async { Err::<(), _>("unmatched") })
            .build();

        // Nothing is decoded on the handler's behalf: a payload that is not
        // even a JSON object still reaches it, meta in hand.
        let mut envelope = envelope_with_action(
            EventKind::Installation,
            Action::Deleted,
            b"not a json object",
        );
        envelope.meta.installation_id = Some(42);
        assert_eq!(
            dispatcher.dispatch(envelope).await,
            Outcome {
                matched: Match::Matched,
                result: Ok(()),
            }
        );
        assert_eq!(
            seen.lock().await.as_slice(),
            [("delivery".to_owned(), Some(42))]
        );

        // The route is by kind and action, so another action of the kind is
        // unmatched and the strict fallback answers.
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(installation_created()).await),
            (Match::UnmatchedAction, Err(AppError::Handler("unmatched")))
        );
    }

    #[tokio::test]
    async fn on_with_a_payload_under_a_matcher_of_another_kind_fails_at_the_kind() {
        /// Keeps the decode error so the test can see which variant it was.
        #[derive(Debug)]
        enum ServiceError {
            Decode(DecodeError),
        }
        impl From<DecodeError> for ServiceError {
            fn from(error: DecodeError) -> Self {
                Self::Decode(error)
            }
        }
        impl From<std::convert::Infallible> for ServiceError {
            fn from(never: std::convert::Infallible) -> Self {
                match never {}
            }
        }

        // The bytes of a check run would fit this view over any object; the
        // kind the view declares is what disagrees with the matcher.
        let calls = Calls::default();
        let always_calls = Arc::clone(&calls);
        let handler_calls = Arc::clone(&calls);
        let builder = Dispatcher::<ServiceError>::builder().always(move |_: Envelope| {
            let calls = Arc::clone(&always_calls);
            async move {
                calls.lock().await.push("always");
                Ok::<_, std::convert::Infallible>(())
            }
        });
        let registration_site = line!() + 1;
        let dispatcher = builder.on(
            EventKind::CheckRun,
            move |_: EventMeta, _: AnyPullRequest| {
                let calls = Arc::clone(&handler_calls);
                async move {
                    calls.lock().await.push("routed");
                    Ok::<_, std::convert::Infallible>(())
                }
            },
        );

        let error = dispatcher
            .build()
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap_err();

        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.registration_site.line(), registration_site);
        assert!(
            matches!(
                error.source,
                ServiceError::Decode(DecodeError::KindMismatch {
                    expected: EventKind::PullRequest,
                    actual: EventKind::CheckRun,
                })
            ),
            "{:?}",
            error.source
        );
        assert_eq!(calls.lock().await.as_slice(), ["always"]);
    }

    #[tokio::test]
    async fn on_with_a_payload_under_its_own_kind_decodes_it_as_on_payload_does() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(EventKind::PullRequest, record_payload(&calls, "on"))
            .on_payload(record_payload(&calls, "on_payload"))
            .on(
                (EventKind::PullRequest, [Action::Opened, Action::Reopened]),
                record_payload(&calls, "on-actions"),
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();

        assert_eq!(
            calls.lock().await.as_slice(),
            ["on-actions", "on", "on_payload"]
        );
    }

    #[tokio::test]
    async fn on_routes_a_consumer_from_envelope_view_under_several_kinds() {
        use crate::FromEnvelope;

        /// The sender's login, which every kind's payload carries; not a
        /// `Payload`, since it declares no single kind.
        #[derive(serde::Deserialize)]
        struct Sender {
            sender: Login,
        }
        #[derive(serde::Deserialize)]
        struct Login {
            login: String,
        }
        impl FromEnvelope for Sender {
            fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
                envelope.decode()
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                [EventKind::PullRequest, EventKind::CheckRun],
                move |meta: EventMeta, sender: Sender| {
                    let seen = Arc::clone(&handler_seen);
                    async move {
                        seen.lock().await.push((meta.kind, sender.sender.login));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .fallback(|_: Envelope| async { Err::<(), _>("unmatched") })
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
        // A kind the view was not registered under is unmatched as usual.
        assert_eq!(
            unwrapped(dispatcher.dispatch(installation_created()).await),
            Err(AppError::Handler("unmatched"))
        );

        assert_eq!(
            seen.lock().await.as_slice(),
            [
                (EventKind::PullRequest, "gagbo".to_owned()),
                (EventKind::CheckRun, "Codertocat".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn an_arc_shared_webhook_handler_is_registered_as_itself() {
        use crate::WebhookHandler;

        /// A handler the test keeps a handle on after registering it, to read
        /// what it saw without a closure adapter.
        struct Auditor {
            seen: Mutex<Vec<EventKind>>,
        }

        impl WebhookHandler for Auditor {
            type Error = std::convert::Infallible;

            async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
                self.seen.lock().await.push(envelope.meta.kind);
                Ok(())
            }
        }

        let auditor = Arc::new(Auditor {
            seen: Mutex::new(Vec::new()),
        });
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(Arc::clone(&auditor))
            .fallback(Arc::clone(&auditor))
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();

        // One instance, two registrations, and the test's own handle.
        assert_eq!(
            auditor.seen.lock().await.as_slice(),
            [EventKind::PullRequest, EventKind::PullRequest]
        );
    }

    #[tokio::test]
    async fn an_arc_shared_payload_handler_is_registered_as_itself() {
        use crate::PayloadHandler;

        struct Labeler {
            seen: Mutex<Vec<(Option<Action>, &'static str)>>,
        }

        impl PayloadHandler<AnyPullRequest> for Labeler {
            type Error = std::convert::Infallible;

            async fn handle(&self, meta: EventMeta, _: AnyPullRequest) -> Result<(), Self::Error> {
                self.seen.lock().await.push((meta.action, "payload"));
                Ok(())
            }
        }

        impl PayloadHandler<()> for Labeler {
            type Error = std::convert::Infallible;

            async fn handle(&self, meta: EventMeta, (): ()) -> Result<(), Self::Error> {
                self.seen.lock().await.push((meta.action, "meta"));
                Ok(())
            }
        }

        let labeler = Arc::new(Labeler {
            seen: Mutex::new(Vec::new()),
        });
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload::<AnyPullRequest, _>(Arc::clone(&labeler))
            .on::<(), _>(
                (EventKind::PullRequest, Action::Closed),
                Arc::clone(&labeler),
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        dispatcher
            .dispatch(pull_request(Action::Closed))
            .await
            .result
            .unwrap();

        assert_eq!(
            labeler.seen.lock().await.as_slice(),
            [
                (Some(Action::Opened), "payload"),
                (Some(Action::Closed), "meta"),
                (Some(Action::Closed), "payload"),
            ]
        );
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
            unwrapped(dispatcher.dispatch(future).await),
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
        assert_eq!(dispatcher.dispatch(future.clone()).await.result, Ok(()));
        assert_eq!(calls.lock().await.as_slice(), ["view"]);

        // The same handler registered for every action of the kind is asked,
        // and the delivery fails at its decode.
        let kind_wide = Dispatcher::<AppError>::builder()
            .on_payload(|_: EventMeta, _: PullRequestWebhookEventPayload| async {
                Ok::<_, std::convert::Infallible>(())
            })
            .build();
        assert_eq!(
            unwrapped(kind_wide.dispatch(future).await),
            Err(AppError::Decode)
        );
    }

    #[tokio::test]
    async fn every_matcher_form_expands_to_its_routes() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                [EventKind::PullRequest, EventKind::CheckRun],
                record_meta(&calls, "kinds"),
            )
            .on(
                (EventKind::PullRequest, [Action::Opened, Action::Closed]),
                record_meta(&calls, "actions"),
            )
            .on(
                [
                    (EventKind::PullRequest, Action::Opened),
                    (EventKind::CheckRun, Action::Completed),
                ],
                record_meta(&calls, "pairs"),
            )
            .on(
                EventMatcher::from(EventKind::Installation)
                    .or((EventKind::CheckRun, Action::Completed)),
                record_meta(&calls, "or"),
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["actions", "pairs", "kinds"]);

        calls.lock().await.clear();
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["pairs", "or", "kinds"]);

        calls.lock().await.clear();
        dispatcher
            .dispatch(installation_created())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["or"]);

        // The action list is exact: a pull request being synchronized does not
        // reach the [opened, closed] handler.
        calls.lock().await.clear();
        let synchronized = envelope_with_action(
            EventKind::PullRequest,
            Action::Synchronize,
            include_bytes!("../tests/fixtures/pull_request.opened.json"),
        );
        dispatcher.dispatch(synchronized).await.result.unwrap();
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

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(
            seen.lock().await.as_slice(),
            [(
                "delivery".to_owned(),
                2,
                PullRequestWebhookEventAction::Opened
            )]
        );

        // Another kind never reaches it, and with no fallback still succeeds.
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
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

        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();

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
            .always(|envelope: Envelope| async move {
                if envelope.meta.kind == EventKind::Installation {
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
            unwrapped(dispatcher.dispatch(installation_created()).await),
            Err(ServiceError::Db(DbError))
        );
        assert_eq!(
            unwrapped(dispatcher.dispatch(check_run_completed()).await),
            Err(ServiceError::Api(ApiError))
        );
        assert_eq!(
            unwrapped(dispatcher.dispatch(pull_request_opened()).await),
            Err(ServiceError::Queue(QueueError))
        );
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn an_event_decode_failure_stops_the_delivery_at_the_first_handler_over_webhook_event() {
        // The always tier and payload handlers over a consumer view sit either
        // side of the handlers over `WebhookEvent`, so the log shows exactly
        // where the chain stopped: each `WebhookEvent` route decodes its own
        // input when it runs, and nothing before the first one needed octocrab.
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
            unwrapped(dispatcher.dispatch(unrepresentable()).await),
            Err(AppError::Decode)
        );
        assert_eq!(calls.lock().await.as_slice(), ["always", "payload-before"]);
    }

    #[tokio::test]
    async fn a_delivery_with_only_webhook_and_payload_handlers_never_decodes_the_event() {
        // The same unrepresentable payload succeeds when no handler over
        // `WebhookEvent` needs octocrab's decoding: the always and fallback
        // tiers see the envelope as verified, and a consumer view over the
        // bytes has nothing octocrab must represent.
        let calls = Calls::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(record(&calls, "always"))
            .on_payload(record_payload(&calls, "payload"))
            .fallback(fail(&calls, "unmatched"))
            .build();

        assert_eq!(dispatcher.dispatch(unrepresentable()).await.result, Ok(()));
        assert_eq!(calls.lock().await.as_slice(), ["always", "payload"]);
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
            unwrapped(always.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("always"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["always"]);

        calls.lock().await.clear();
        let fallback = Dispatcher::<AppError>::builder()
            .fallback(fail(&calls, "fallback"))
            .fallback(record(&calls, "fallback-after"))
            .build();
        assert_eq!(
            unwrapped(fallback.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("fallback"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["fallback"]);
    }

    #[tokio::test]
    async fn the_routed_chains_fail_fast() {
        let calls = Calls::default();
        let routed = Dispatcher::<AppError>::builder()
            .on(
                (EventKind::PullRequest, Action::Opened),
                fail_meta(&calls, "action"),
            )
            .on(
                (EventKind::PullRequest, Action::Opened),
                record_meta(&calls, "action-after"),
            )
            .on(EventKind::PullRequest, record_meta(&calls, "kind"))
            .build();
        assert_eq!(
            unwrapped(routed.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("action"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["action"]);

        calls.lock().await.clear();
        let kind_wide = Dispatcher::<AppError>::builder()
            .on(EventKind::PullRequest, fail_meta(&calls, "kind"))
            .on(EventKind::PullRequest, record_meta(&calls, "kind-after"))
            .build();
        assert_eq!(
            unwrapped(kind_wide.dispatch(pull_request_opened()).await),
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

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();

        assert_eq!(calls.lock().await.as_slice(), ["pull-request", "check-run"]);
    }
}
