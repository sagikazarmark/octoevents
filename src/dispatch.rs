use std::{
    any::type_name, collections::HashMap, error::Error, fmt, marker::PhantomData, panic::Location,
    sync::Arc,
};

use crate::{
    Action, BoxError, Envelope, EventKind, EventMeta, FromEnvelope, Handler, IntoMatcher,
    MaybeSend, MaybeSync, matcher::Slot, runtime::BoxFuture, trace,
};

/// The erased handler: every handler is registered as one of these, its
/// input's decode folded in and its error converted into [`BoxError`], so
/// routing is monomorphic.
///
/// A trait rather than a `dyn Fn` over the envelope, for two reasons a
/// function signature cannot give. The future may borrow the handler, so
/// the decode happens before the future exists and the decoded input goes
/// straight into `Handler::handle`: a `'static` future built by a closure
/// would have to capture the input, and the input would need to be
/// `MaybeSend`, a bound `on` does not place. And the platform split is the
/// supertraits, as on `Handler`, rather than a hand-written pair of aliases.
trait ErasedHandler: MaybeSend + MaybeSync {
    /// Decodes the handler's input from the envelope and starts the handler
    /// on it.
    ///
    /// The envelope is borrowed for the decode alone: nothing is cloned for
    /// a route whose input is not the envelope, and a route over the
    /// envelope clones it once, through `FromEnvelope`. A decode failure is
    /// the `Err`, already boxed, so no future is built for a delivery the
    /// handler cannot receive; the `Ok` is the handler's future, its error
    /// boxed on completion.
    fn call<'a>(
        &'a self,
        envelope: &Envelope,
    ) -> Result<BoxFuture<'a, Result<(), BoxError>>, BoxError>;
}

/// A routed handler behind its input's decode.
///
/// `fn(I)` rather than `I`: the route is `MaybeSend + MaybeSync` when the
/// handler is, whatever the input.
struct Routed<I, H> {
    handler: H,
    input: PhantomData<fn(I)>,
}

impl<I, H> ErasedHandler for Routed<I, H>
where
    I: FromEnvelope,
    H: Handler<I> + MaybeSend + MaybeSync,
    H::Error: Into<BoxError>,
{
    fn call<'a>(
        &'a self,
        envelope: &Envelope,
    ) -> Result<BoxFuture<'a, Result<(), BoxError>>, BoxError> {
        let input = I::from_envelope(envelope).map_err(BoxError::from)?;
        let future = self.handler.handle(input);
        Ok(Box::pin(async move { future.await.map_err(Into::into) }))
    }
}

/// A handler over the envelope for the `always` and `fallback` tiers, whose
/// input is known to be the envelope: it is cloned once, here, without going
/// through `Envelope::from_envelope`, so those two tiers decode nothing. A
/// detail of those two tiers: the consumer's handler is the same
/// `Handler<Envelope>` as anywhere.
struct OverEnvelope<H> {
    handler: H,
}

impl<H> ErasedHandler for OverEnvelope<H>
where
    H: Handler<Envelope> + MaybeSend + MaybeSync,
    H::Error: Into<BoxError>,
{
    fn call<'a>(
        &'a self,
        envelope: &Envelope,
    ) -> Result<BoxFuture<'a, Result<(), BoxError>>, BoxError> {
        let future = self.handler.handle(envelope.clone());
        Ok(Box::pin(async move { future.await.map_err(Into::into) }))
    }
}

/// A handler that routes verified envelopes to other handlers by kind and
/// action.
///
/// Per delivery the dispatcher runs three tiers in the order [`Tier`] lists
/// them. The `always` chain runs first, for every delivery, and receives the
/// verified [`Envelope`], bytes included. The routed chains run next: the
/// chain for the envelope's kind and action, then the kind-wide chain. Every
/// routed handler is a [`Handler`] over some [`FromEnvelope`] input,
/// registered with `on` for the kinds and actions a matcher selects; a
/// handler over a [`Payload`](crate::Payload) may give actions alone and take
/// the kind from its type. The `fallback` chain runs only if neither
/// routed chain matched, and receives the envelope as `always` does.
/// Every chain is sequential, in registration order, and stops at the first
/// error. `always` and `fallback` never count as a match, and an empty
/// fallback chain succeeds, so unmatched kinds are green in GitHub until you
/// decide otherwise.
///
/// There are no priorities and no propagation control: a handler cannot be
/// moved ahead of one registered earlier, and cannot stop the chain or
/// declare a delivery "not mine" so that a later handler takes it. The tiers
/// plus registration order cover what a webhook receiver needs, and matching
/// decided by handlers at run time would leave the route table unable to say
/// what it routes; the designs this was weighed against are recorded in the
/// repository's design notes, linked from the crate docs under
/// [Design](crate#design). Whether a delivery is routed at all is not a
/// tier's decision either; it is made outside the dispatcher, at
/// [the policy seam](#the-policy-seam).
///
/// [`dispatch`](Self::dispatch) reports an [`Outcome`]: whether the delivery
/// was matched, and if not, whether its kind was known to the route table,
/// beside the result of the handlers that ran. The outcome is distinct from
/// success: a matched delivery can fail, and an unmatched one can succeed. A
/// delivery matches when at least one routed handler is registered for its
/// kind, or its kind and action; matching is decided by the route table,
/// never by a handler, and the `always` tier does not match. As a
/// [`Handler<Envelope>`] the dispatcher keeps only the result, so the receiver
/// sees an unmatched delivery as a success unless a fallback failed it; the
/// outcome is for the policy seam to read.
///
/// A failure is reported as a [`DispatchError`]: the handler's error, boxed
/// as a [`BoxError`], wrapped with the [`Tier`] the failing handler ran in,
/// the delivery's ID, kind and action, the handler's name, and the source
/// location of the registration that put the handler there. Every
/// registration method records its handler's name and its caller's location,
/// so an operator reading "delivery X failed" knows which handler and can go
/// to the line of code that registered it.
///
/// The decode rule: `always` and `fallback` receive the bytes as they were
/// verified and nothing is decoded on their behalf; each routed handler
/// decodes its own input, through [`FromEnvelope`], when its route runs. So a
/// payload octocrab cannot represent still reaches `always`, every handler
/// over a consumer view or the [`EventMeta`] alone, and a strict `fallback`,
/// which answers it with its own error rather than a decode error; the
/// delivery fails only at the first handler over octocrab's `WebhookEvent`,
/// and the [`DispatchError`] names that registration. A routed handler
/// decodes only when its route matches: a handler registered for some actions
/// decodes nothing for a delivery carrying another. Routing itself decodes
/// nothing either: the kind and action a route is looked up by were read into
/// the [`EventMeta`] when the envelope was built, the kind from the header and
/// the action from the payload's top level.
///
#[cfg_attr(feature = "derive", doc = "```")]
#[cfg_attr(not(feature = "derive"), doc = "```ignore")]
/// use octoevents::{Action, AnyAction, BoxError, Dispatcher, Envelope, Event, EventKind};
///
/// /// A fallback's own reason for failing a delivery.
/// #[derive(Debug, thiserror::Error)]
/// #[error("no handler for {0} events")]
/// struct Unhandled(EventKind);
///
/// // A consumer view over the pull-request payload; the kind it declares is
/// // the kind its handler is routed by.
/// #[derive(serde::Deserialize, octoevents::Payload)]
/// #[payload(EventKind::PullRequest)]
/// struct PullRequestNumber { number: u64 }
///
/// async fn forward(envelope: Envelope) -> Result<(), BoxError> {
///     println!("forward {} ({} bytes)", envelope.meta.delivery_id, envelope.raw_payload.len());
///     Ok(())
/// }
///
/// async fn notify(Event { meta, payload }: Event<PullRequestNumber>) -> Result<(), BoxError> {
///     println!("PR #{} {:?} for installation {:?}", payload.number, meta.action, meta.installation_id);
///     Ok(())
/// }
///
/// async fn label(pr: PullRequestNumber) -> Result<(), std::io::Error> {
///     println!("label PR #{}", pr.number);
///     Ok(())
/// }
///
/// async fn reject(envelope: Envelope) -> Result<(), Unhandled> {
///     Err(Unhandled(envelope.meta.kind))
/// }
///
/// let dispatcher = Dispatcher::builder()
///     .always(forward)
///     .on(AnyAction, notify)
///     .on([Action::Opened, Action::Reopened], label)
///     .fallback(reject)
///     .build();
/// # let _ = dispatcher;
/// ```
///
/// Each handler keeps its own error type, and every registration method asks
/// the same one thing of it, `Into<BoxError>`: every `Error + Send + Sync +
/// 'static` type is, through std's blanket `From`, and so are `BoxError`
/// itself, `anyhow::Error`, `String`, `&str` and
/// [`Infallible`](std::convert::Infallible). The dispatcher boxes the error
/// where the handler is registered, so the handlers above share no error
/// enum and a reusable struct handler registers with the error it has. A
/// decode failure is boxed the same way, as the [`DecodeError`] it is, and
/// reported at the handler that needed the decode.
///
/// [`DecodeError`]: crate::DecodeError
///
/// `on` routes a handler over any [`FromEnvelope`] input for the kinds and
/// actions a matcher selects. [`EventMeta`] decodes nothing, so a handler
/// over it is routed by kind and action and receives only the meta;
/// [`Envelope`](crate::Envelope) hands over the bytes for one kind, as
/// `always` does for every kind; a consumer type implementing `FromEnvelope`
/// itself is a view over fields several kinds share. None of them needs
/// octocrab.
///
/// ```
/// use octoevents::{Action, BoxError, DecodeError, Dispatcher, Envelope, EventKind, EventMeta, FromEnvelope};
///
/// #[derive(serde::Deserialize)]
/// struct Sender { sender: Login }
/// #[derive(serde::Deserialize)]
/// struct Login { login: String }
///
/// impl FromEnvelope for Sender {
///     fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
///         envelope.decode()
///     }
/// }
///
/// async fn revoke(meta: EventMeta) -> Result<(), BoxError> {
///     println!("revoke tokens for installation {:?}", meta.installation_id);
///     Ok(())
/// }
///
/// async fn forward(envelope: Envelope) -> Result<(), BoxError> {
///     println!("forward {} bytes of {}", envelope.raw_payload.len(), envelope.meta.kind);
///     Ok(())
/// }
///
/// async fn metrics(sender: Sender) -> Result<(), BoxError> {
///     println!("by {}", sender.sender.login);
///     Ok(())
/// }
///
/// let dispatcher = Dispatcher::builder()
///     .on((EventKind::Installation, Action::Deleted), revoke)
///     .on(EventKind::Push, forward)
///     .on([EventKind::Issues, EventKind::IssueComment], metrics)
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
/// use octoevents::{Action, BoxError, Dispatcher, Event, EventKind};
///
/// async fn triage(Event { meta, payload: event }: Event<WebhookEvent>) -> Result<(), BoxError> {
///     println!("triage {:?} for {:?}", meta.action, event.repository.map(|repository| repository.name));
///     Ok(())
/// }
///
/// let dispatcher = Dispatcher::builder()
///     .on((EventKind::PullRequest, [Action::Opened, Action::Synchronize]), triage)
///     .build();
/// # let _ = dispatcher;
/// # }
/// ```
///
/// # The policy seam
///
/// A tier can continue or fail, never skip. An `always` handler that cannot
/// store an envelope fails the delivery, as it should; one that finds the
/// delivery ID already stored cannot answer the redelivery with success and
/// keep it from being routed. A `fallback` handler receives the envelope and
/// not the match, so it cannot tell a kind the route table never registered
/// from an action GitHub added to a kind it did, and it can only fail the
/// delivery or let it pass. A tier that could skip would make "matched" the
/// run-time decision of one handler rather than a property of the route
/// table, and neither the [`Outcome`] nor a strict fallback could then say
/// what it reports or rejects.
///
/// The policy the tiers cannot express belongs in a [`Handler<Envelope>`]
/// that wraps the dispatcher and calls [`dispatch`](Self::dispatch) itself.
/// It persists the envelope first; answers a redelivery of a stored delivery
/// ID with success without calling `dispatch`; and reads the [`Outcome`] to
/// dead-letter or forward an unmatched delivery, bytes still in hand, without
/// failing it, or to reject a kind the route table does not know while
/// tolerating an added action. What the wrapper answers before `dispatch`
/// reaches no tier, `always` included. The dispatcher only routes.
/// [`Outcome`]'s docs show a wrapper that dead-letters an unknown kind; the
/// `dispatcher` example shows one that also persists and deduplicates. The
/// [design notes](crate#design) record the short-circuit tier this replaces.
#[derive(Clone)]
pub struct Dispatcher {
    routes: Arc<Routes>,
}

impl fmt::Debug for Dispatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.routes.fmt_as("Dispatcher", formatter)
    }
}

impl Dispatcher {
    /// Starts building a dispatcher whose unmatched deliveries succeed.
    #[must_use]
    pub fn builder() -> DispatcherBuilder {
        DispatcherBuilder::default()
    }

    /// Runs the `always` chain, the matching routed chains, and the fallback
    /// chain when nothing matched, in that order, and reports the
    /// [`Outcome`].
    ///
    /// The outcome carries the match the route table decided and the result
    /// of the handlers that ran: the first handler error, or the decode error
    /// of the first handler whose input could not be decoded, each wrapped in
    /// a [`DispatchError`] naming the tier it came from, the delivery, the
    /// handler, and where it was registered. The two are independent: a
    /// matched delivery can fail, and an unmatched one succeeds unless a
    /// fallback fails it. [`Handler::handle`] on the dispatcher keeps only
    /// the result.
    ///
    /// A plain `async fn` with no runtime of its own: a transport awaits it
    /// on whatever executor it has, and a synchronous entry with none polls
    /// it once. When no handler suspends, a single poll with a no-op waker
    /// completes it; a poll that comes back `Pending` means a handler did
    /// suspend and needs an executor after all.
    ///
    /// ```
    /// use std::pin::pin;
    /// use std::task::{Context, Poll, Waker};
    ///
    /// use octoevents::{BoxError, Dispatcher, Envelope, EventKind};
    ///
    /// async fn log(envelope: Envelope) -> Result<(), BoxError> {
    ///     println!("{} bytes of {}", envelope.raw_payload.len(), envelope.meta.kind);
    ///     Ok(())
    /// }
    ///
    /// let dispatcher = Dispatcher::builder().on(EventKind::Push, log).build();
    /// let envelope = Envelope::new(
    ///     "72d3162e-cc78-11e3-81ab-4c9367dc0958",
    ///     EventKind::Push,
    ///     br#"{"ref":"refs/heads/main"}"#,
    /// );
    ///
    /// let mut future = pin!(dispatcher.dispatch(envelope));
    /// let Poll::Ready(outcome) = future.as_mut().poll(&mut Context::from_waker(Waker::noop())) else {
    ///     panic!("a handler suspended; drive the future on an executor");
    /// };
    /// assert!(outcome.result.is_ok());
    /// ```
    ///
    /// With the `tracing` feature, the call runs in an `octoevents.dispatch`
    /// span that records `delivery_id`, `event`, and, when the delivery has
    /// them, `action` and `installation_id`, all on open. On the way out it
    /// records `outcome` as one of four labels: `ok` (matched, every handler
    /// succeeded), `handler_error` (matched, a handler failed),
    /// `unmatched_ok` (nothing routed matched, no fallback failed) and
    /// `unmatched_error` (nothing routed matched, a handler failed, in
    /// whichever tier). On failure it also records `tier`, `handler` and
    /// `registration_site`, the [`DispatchError`]'s, so the span alone says
    /// which handler failed the delivery. The crate's tracing contract as a
    /// whole is under [Tracing](crate#tracing).
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
                handler = tracing::field::Empty,
                registration_site = tracing::field::Empty,
            )
        )
    )]
    pub async fn dispatch(&self, envelope: Envelope) -> Outcome {
        let (matched, routed) = self.routes.lookup(&envelope.meta);
        let result = self.run_tiers(&envelope, matched, routed).await;
        let outcome = Outcome { matched, result };
        trace::record("outcome", outcome.label());
        if let Err(error) = &outcome.result {
            trace::record("tier", error.tier.as_str());
            trace::record("handler", error.handler);
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
        routed: impl Iterator<Item = &[Route]>,
    ) -> Result<(), DispatchError> {
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
/// the tier, the delivery, and the failing route's handler name and
/// registration site. The clones for the error happen only on that path.
async fn run_chain(envelope: &Envelope, tier: Tier, chain: &[Route]) -> Result<(), DispatchError> {
    for route in chain {
        // Two failure points, one shape: a decode failure before the future
        // exists, the handler's after it ran.
        let future = route
            .handler
            .call(envelope)
            .map_err(|decode| route.failed(tier, envelope, decode))?;
        future
            .await
            .map_err(|source| route.failed(tier, envelope, source))?;
    }
    Ok(())
}

impl Handler<Envelope> for Dispatcher {
    type Error = DispatchError;

    /// Dispatches the envelope and keeps only the result: an unmatched
    /// delivery succeeds unless a fallback fails it. The `octoevents.dispatch`
    /// span records the outcome on this path too.
    ///
    /// This is what lets a dispatcher be the receiver's handler, and what a
    /// wrapper at [the policy seam](Dispatcher#the-policy-seam) calls around
    /// instead, to read the [`Outcome`] this discards. It also lets a
    /// dispatcher be a route of another, and then it is a handler like any
    /// other: it contributes a result, never a match. The outer outcome
    /// reports the outer route table's decision alone, `Matched` for a kind
    /// the inner dispatcher had no route for; each dispatcher's span records
    /// its own outcome. The inner [`DispatchError`] is an
    /// [`Error`](std::error::Error) like any handler's, so it is boxed as the
    /// outer error's source, and a reporter walking the chain reads the outer
    /// site, then the inner, then the application error.
    ///
    /// ```
    /// use octoevents::{Dispatcher, EventKind};
    ///
    /// let inner = Dispatcher::builder().build();
    /// let outer = Dispatcher::builder()
    ///     .on(EventKind::Issues, inner)
    ///     .build();
    /// # let _ = outer;
    /// ```
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
/// wrapped in a [`DispatchError`] that names the tier, the delivery, the
/// failing handler and where it was registered. A matched delivery can fail;
/// an unmatched one succeeds unless a fallback fails it.
///
/// A handler wrapping a [`Dispatcher`] reads both to set the policy the
/// tiers cannot, as [The policy seam](Dispatcher#the-policy-seam) describes;
/// the receiver never sees this type, since [`Handler::handle`] on the
/// dispatcher returns `result` alone. Nor does an outer dispatcher see a
/// nested one's: the inner outcome is on the inner span, and the outer
/// reports its own route table's match.
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
/// `unmatched_error` with no fallback registered. The tier, the handler and
/// the registration site are fields of their own on the same span.
///
/// The dispatcher produces this and consumers only read it, so it is
/// `#[non_exhaustive]` for the reason [`DispatchError`] is: another field
/// can be added without that becoming a breaking change here. A consumer's
/// test that needs one dispatches, and compares the fields it cares about.
///
/// ```compile_fail,E0639
/// use octoevents::{Match, Outcome};
///
/// let outcome = Outcome {
///     matched: Match::Matched,
///     result: Ok(()),
/// };
/// ```
///
/// ```
/// use octoevents::{DispatchError, Dispatcher, Envelope, Handler, Match};
///
/// /// Dead-letters deliveries of kinds the dispatcher never registered.
/// struct DeadLetter {
///     dispatcher: Dispatcher,
/// }
///
/// impl Handler<Envelope> for DeadLetter {
///     // The dispatcher's error passes through, tier, handler and registration site included.
///     type Error = DispatchError;
///
///     async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
///         // The clone shares the bytes; the wrapper still holds them.
///         let outcome = self.dispatcher.dispatch(envelope.clone()).await;
///         match outcome.matched {
///             // Routed, or an action GitHub added to a kind this app handles.
///             Match::Matched | Match::UnmatchedAction => outcome.result,
///             Match::UnmatchedKind => {
///                 outcome.result?;
///                 println!("dead-letter {} ({} bytes)", envelope.meta.delivery_id, envelope.raw_payload.len());
///                 Ok(())
///             }
///         }
///     }
/// }
/// # let _ = DeadLetter { dispatcher: Dispatcher::builder().build() };
/// ```
#[derive(Debug)]
#[must_use = "an outcome carries the handlers' result in its `result` field"]
#[non_exhaustive]
pub struct Outcome {
    /// Whether the route table matched the delivery, and if not, whether it
    /// knew the kind.
    pub matched: Match,
    /// `Ok` when every handler that ran succeeded; otherwise the first error,
    /// with the tier, handler and registration site it came from.
    pub result: Result<(), DispatchError>,
}

impl Outcome {
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
/// count: a delivery that reaches only them is unmatched. When nothing
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

impl Match {
    /// The match as a label: `matched`, `unmatched_action` or
    /// `unmatched_kind`, in the `snake_case` the dispatch span's `outcome`
    /// labels use, so a policy that logs which it saw beside them reads as
    /// one vocabulary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::UnmatchedAction => "unmatched_action",
            Self::UnmatchedKind => "unmatched_kind",
        }
    }
}

impl fmt::Display for Match {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The error of a failed dispatch: the application error, boxed, and where in
/// the dispatch and in the consumer's source it came from.
///
/// The dispatcher wraps the error of the handler that failed the delivery
/// with what it knew and the handler did not: the [`Tier`] the handler ran
/// in, the delivery's ID, kind and action, the handler's name, and the
/// source location of the registration (`always`, `on` or `fallback`) that
/// put the handler there. Every
/// registration method records its caller's location and its handler's name
/// at compile time, so each costs one static reference per registration, on
/// `wasm32` as anywhere. A decode failure is reported at the handler that
/// needed the decode: its tier, its name, its registration site, and the
/// [`DecodeError`](crate::DecodeError) as the source.
///
/// The handler name is [`type_name`]'s output for the type the registration
/// method received: the function's path for an `async fn` item
/// (`app::revoke`), the struct's path for a struct handler
/// (`app::Revoker`), and the enclosing function's path with a `{{closure}}`
/// suffix for a closure (`app::main::{{closure}}`), so a closure is found by
/// its registration site, a named handler by its name. `type_name` gives no
/// stability guarantee, so the string is for an operator to read, not for
/// code to match on; a policy that keys on the failing handler compares
/// [`registration_site`](Self::registration_site).
///
/// [`Display`](fmt::Display) names where, not why: the tier, the delivery,
/// the handler, and the registration site. Why is the
/// [`source`](Error::source), the application error, so a reporter that walks
/// the chain prints both; [`into_source`](Self::into_source) drops the
/// wrapping for code that wants the application error alone, and code that
/// wants its own type back downcasts the [`BoxError`]:
/// `error.source.downcast_ref::<AppError>()`, or, for a decode failure,
/// `downcast_ref::<DecodeError>()`. The type is an [`Error`] whatever the
/// handler's error was, since the source is always the box, so a dispatcher
/// nests as a route of another and the receiver puts the error on the
/// failed-delivery event with no bound left to ask.
///
/// A wrapping handler that passes the dispatcher's result through keeps the
/// tier, handler name and registration site by making this its error type;
/// the receiver accepts it as it does any error, and hands it to the
/// observer registered with `WebhookReceiverBuilder::on_error` before
/// answering 500.
///
/// The dispatcher produces this and consumers only read it, so it is
/// `#[non_exhaustive]`: another field can be added without that becoming a
/// breaking change here. A test that needs one dispatches to a handler that
/// fails.
///
/// ```text
/// delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (pull_request.opened) failed in the route tier at the handler `app::label` registered at src/main.rs:42:10
///   caused by: database is down
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct DispatchError {
    /// The tier the failing handler ran in.
    pub tier: Tier,
    /// The failing handler's name: [`type_name`] of the handler the
    /// registration method received, as the docs on this type describe.
    pub handler: &'static str,
    /// Where the failing handler was registered: the call to the registration
    /// method in the consumer's source.
    pub registration_site: &'static Location<'static>,
    /// The `X-GitHub-Delivery` value of the delivery that failed.
    pub delivery_id: String,
    /// The kind of the delivery that failed.
    pub kind: EventKind,
    /// The action of the delivery that failed, when it had one.
    pub action: Option<Action>,
    /// The application error, boxed: the handler's own, or the
    /// [`DecodeError`](crate::DecodeError) when the handler's input could not
    /// be decoded. What [`source`](Error::source) returns, by value.
    pub source: BoxError,
}

impl DispatchError {
    /// Drops the wrapping and returns the application error.
    ///
    /// The one-call path from a dispatch result to the boxed application
    /// error, for code that reports the delivery and the handler by other
    /// means, or hands the error on as its own.
    #[must_use]
    pub fn into_source(self) -> BoxError {
        self.source
    }
}

// Written out rather than derived through thiserror: the action is optional
// and joins the kind with a dot only when present (`pull_request.opened`,
// `ping`), which a format string cannot express.
impl fmt::Display for DispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "delivery {} ({}", self.delivery_id, self.kind)?;
        if let Some(action) = &self.action {
            write!(formatter, ".{action}")?;
        }
        write!(
            formatter,
            ") failed in the {} tier at the handler `{}` registered at {}",
            self.tier, self.handler, self.registration_site
        )
    }
}

impl Error for DispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&*self.source)
    }
}

/// The tiers a [`Dispatcher`] runs a delivery through, in order.
///
/// Named by a [`DispatchError`] to say which one the failing handler ran in.
/// The three are the dispatcher's definition, so a policy matches on them
/// without a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// The `always` chain: handlers over the envelope, bytes included, that
    /// run for every delivery before routing.
    Always,
    /// The routed chains: the handlers `on` registered for the delivery's
    /// kind and action.
    Route,
    /// The `fallback` chain: handlers over the envelope that run only when
    /// no routed handler matched.
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
pub struct DispatcherBuilder {
    routes: Routes,
}

impl fmt::Debug for DispatcherBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.routes.fmt_as("DispatcherBuilder", formatter)
    }
}

impl Default for DispatcherBuilder {
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

impl DispatcherBuilder {
    /// Registers a handler over the [`Envelope`] that runs for every delivery
    /// the dispatcher receives, before routing.
    ///
    /// It receives the verified envelope, bytes included, and nothing is
    /// decoded on its behalf, so it runs even for a payload no routed handler
    /// can decode: the tier for audit, metrics, and forwarding. Its failure
    /// fails the delivery, and it never counts as a match, so a strict
    /// fallback still rejects kinds nothing else handles. It can continue or
    /// fail but never skip; a handler that decides whether a delivery is
    /// routed at all wraps the dispatcher instead, as
    /// [The policy seam](Dispatcher#the-policy-seam) describes.
    ///
    /// Every delivery means every one the dispatcher is handed; two things
    /// upstream of it keep some from arriving. The receiver answers a `ping`
    /// itself, before the dispatcher, unless built with
    /// `WebhookReceiverBuilder::handle_ping(true)`. A wrapper that answers a
    /// redelivery of a stored delivery ID with success before calling
    /// `dispatch` keeps that redelivery from this tier too, so a metric that
    /// must count every verified delivery belongs at the top of the wrapper,
    /// not here.
    ///
    /// Like every registration method, this records the handler's name and
    /// where it was called so a [`DispatchError`] can point back at the
    /// registration, and asks `Into<BoxError>` of the handler's error, which
    /// any `Error` is:
    ///
    /// ```
    /// use octoevents::{Dispatcher, Envelope};
    ///
    /// async fn audit(envelope: Envelope) -> Result<(), std::io::Error> {
    ///     println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw_payload.len());
    ///     Ok(())
    /// }
    ///
    /// let dispatcher = Dispatcher::builder().always(audit).build();
    /// # let _ = dispatcher;
    /// ```
    #[must_use]
    #[track_caller]
    pub fn always<H>(mut self, handler: H) -> Self
    where
        H: Handler<Envelope> + MaybeSend + MaybeSync + 'static,
        H::Error: Into<BoxError>,
    {
        self.routes.always.push(Route::over_envelope(handler));
        self
    }

    /// Registers a handler for the kinds and actions the matcher selects.
    ///
    /// The matcher is any [`IntoMatcher`] for the handler's input. One that
    /// says its kinds works for any input: a kind, several kinds, a
    /// `(kind, action)`, a `(kind, [actions])`, `[(kind, action)]` pairs, or an
    /// [`EventMatcher`](crate::EventMatcher) built with
    /// [`or`](crate::EventMatcher::or). For a handler over a
    /// [`Payload`](crate::Payload) `P`, or [`Event<P>`](crate::Event), the
    /// matcher may say actions alone, one [`Action`], an array of them, or
    /// [`AnyAction`](crate::AnyAction) for
    /// every action of the kind, and the kind is `P::KIND`: said once, on the
    /// type, so the handler cannot be registered under another kind. A
    /// pull-request handler under `Action::Opened` is `pull_request.opened`.
    ///
    /// The handler's input is any [`FromEnvelope`], whose docs list the
    /// shipped impls: the [`EventMeta`] alone, for a handler routed by kind
    /// and action that decodes nothing; the [`Envelope`], bytes included, for
    /// one kind's forwarder; a `Payload` or `Event<P>`, which decodes with its
    /// kind check, so a matcher that says a kind the view disagrees with fails
    /// the delivery with [`DecodeError::KindMismatch`](crate::DecodeError::KindMismatch), where a matcher of
    /// actions alone cannot disagree; or a consumer type implementing
    /// `FromEnvelope` itself, for a view over fields several kinds share. Each
    /// route decodes its own input when it runs, and a decode failure fails
    /// the delivery at that position: the [`DispatchError`] names this
    /// registration.
    ///
    #[cfg_attr(feature = "derive", doc = "```")]
    #[cfg_attr(not(feature = "derive"), doc = "```ignore")]
    /// use octoevents::{Action, AnyAction, BoxError, Dispatcher, Event, EventKind, EventMeta};
    ///
    /// #[derive(serde::Deserialize, octoevents::Payload)]
    /// #[payload(EventKind::PullRequest)]
    /// struct PullRequestNumber { number: u64 }
    ///
    /// async fn label(pr: PullRequestNumber) -> Result<(), BoxError> {
    ///     println!("label PR #{}", pr.number);
    ///     Ok(())
    /// }
    ///
    /// async fn notify(Event { meta, payload }: Event<PullRequestNumber>) -> Result<(), BoxError> {
    ///     println!("{}: PR #{} {:?}", meta.delivery_id, payload.number, meta.action);
    ///     Ok(())
    /// }
    ///
    /// async fn revoke(meta: EventMeta) -> Result<(), BoxError> {
    ///     println!("revoke tokens for installation {:?}", meta.installation_id);
    ///     Ok(())
    /// }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .on([Action::Opened, Action::Reopened], label)          // `pull_request`, from the type
    ///     .on(AnyAction, notify)                                  // every `pull_request` action
    ///     .on((EventKind::Installation, Action::Deleted), revoke) // `EventMeta` declares no kind
    ///     .build();
    /// # let _ = dispatcher;
    /// ```
    ///
    /// A route that does not match neither counts as a match nor decodes the
    /// payload: a strict fallback rejects `pull_request.closed` when only
    /// `Action::Opened` is registered, and a payload type that cannot
    /// represent an action (octocrab's per-kind action enums have no
    /// catch-all) fails only the deliveries it was registered for. Under one
    /// kind, routes for an action run before routes for every action. A
    /// handler that needs the action takes `Event<P>` and reads
    /// `meta.action`, the crate's [`Action`], whose [`Unknown`](Action::Unknown)
    /// carries a value this crate does not know.
    ///
    /// A view over fields several kinds share implements `FromEnvelope` itself
    /// and is registered under those kinds:
    ///
    /// ```
    /// use octoevents::{Action, BoxError, DecodeError, Dispatcher, Envelope, EventKind, FromEnvelope};
    ///
    /// #[derive(serde::Deserialize)]
    /// struct Sender { sender: Login }
    /// #[derive(serde::Deserialize)]
    /// struct Login { login: String }
    ///
    /// impl FromEnvelope for Sender {
    ///     fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
    ///         envelope.decode()
    ///     }
    /// }
    ///
    /// async fn forward(envelope: Envelope) -> Result<(), BoxError> {
    ///     println!("forward {} bytes", envelope.raw_payload.len());
    ///     Ok(())
    /// }
    ///
    /// async fn metrics(sender: Sender) -> Result<(), BoxError> {
    ///     println!("by {}", sender.sender.login);
    ///     Ok(())
    /// }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .on(EventKind::Push, forward)
    ///     .on([EventKind::Issues, EventKind::IssueComment], metrics)
    ///     .on([(EventKind::Issues, Action::Opened), (EventKind::PullRequest, Action::Closed)], metrics)
    ///     .build();
    /// # let _ = dispatcher;
    /// ```
    ///
    /// A handler registered under several slots is shared, not duplicated.
    /// `I` is inferred from a closure's parameter type or from a struct that
    /// implements [`Handler`] for one input; a struct that implements it for
    /// several names the input: `on::<EventMeta, _, _>(matcher, auditor)`.
    ///
    /// The handler's error is asked to be `Into<BoxError>`, as every
    /// registration method asks; a decode failure is boxed the same way, as
    /// the [`DecodeError`](crate::DecodeError) it is, so the handler's error type need not know
    /// of decoding. An error type that is not an `Error` and converts into no
    /// box is refused here, with rustc's report on the missing conversion:
    ///
    /// ```compile_fail,E0277
    /// use octoevents::{Dispatcher, Envelope, EventKind};
    ///
    /// // `()` is neither an `Error` nor `Into<BoxError>`.
    /// async fn forward(envelope: Envelope) -> Result<(), ()> { Ok(()) }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .on(EventKind::Push, forward)
    ///     .build();
    /// ```
    ///
    /// Actions alone under an input that declares no kind are refused at
    /// compile time. rustc reports the bound the relative matcher needs, that
    /// the input is a `Payload`, so the message is the `Payload` trait's, and
    /// its first note says to spell the kind (abridged):
    ///
    /// ```text
    /// error[E0277]: `EventMeta` is not a payload
    ///    |
    ///    |     .on(Action::Deleted, revoke)
    ///    |         ^^^^^^^^^^^^^^^ expected a `serde::Deserialize` type that declares the event kind it decodes, or `Event<P>` over one
    ///    |
    ///    = note: `Envelope`, `EventMeta` and a view over several kinds declare no kind: a handler over them is registered with `on` and a matcher that says the kind
    ///    = note: required for `Action` to implement `IntoMatcher<EventMeta>`
    /// ```
    ///
    /// A matcher of the wrong type altogether, a string route among them, is
    /// reported as not a matcher, with every accepted shape and the typed
    /// spelling of `issues.opened` (abridged):
    ///
    /// ```text
    /// error[E0277]: `&str` is not a matcher
    ///    |
    ///    |     .on("issues.opened", forward)
    ///    |      ^^ expected a kind, a `(kind, action)`, a `(kind, [actions])`, `[(kind, action)]` pairs or an `EventMatcher`, or, for a handler over a `Payload`, an `Action`, `[Action; N]` or `AnyAction`
    ///    |
    ///    = note: there is no string form: `issues.opened` is `(EventKind::Issues, Action::Opened)`, or `Action::Opened` alone for a handler over an `issues` payload type
    /// ```
    ///
    /// ```compile_fail,E0277
    /// use octoevents::{BoxError, Dispatcher, Envelope};
    ///
    /// async fn forward(envelope: Envelope) -> Result<(), BoxError> { Ok(()) }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .on("issues.opened", forward)
    ///     .build();
    /// ```
    ///
    /// ```compile_fail,E0277
    /// use octoevents::{Action, BoxError, Dispatcher, EventMeta};
    ///
    /// async fn revoke(meta: EventMeta) -> Result<(), BoxError> { Ok(()) }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .on(Action::Deleted, revoke)
    ///     .build();
    /// ```
    ///
    /// A type that is neither a `Payload` nor a `FromEnvelope` is reported
    /// with both routes to becoming an input (abridged):
    ///
    /// ```text
    /// error[E0277]: `Sender` cannot be decoded from an `Envelope`
    ///    |
    ///    |     .on(EventKind::Issues, |sender: Sender| async move {
    ///    |      ^^ expected `Envelope`, `EventMeta`, a `Payload`, `Event<P>`, or a type that implements `FromEnvelope` itself
    ///    |
    ///    = note: for a serde view over one kind, declare the kind on the type with `#[derive(Payload)] #[payload(EventKind::..)]`: every serde `Payload` is a `FromEnvelope`
    ///    = note: for a view over several kinds, implement `FromEnvelope` for `Sender` directly, decoding with `Envelope::decode`
    /// ```
    ///
    /// ```compile_fail,E0277
    /// use octoevents::{BoxError, Dispatcher, EventKind};
    ///
    /// #[derive(serde::Deserialize)]
    /// struct Sender { sender: String }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .on(EventKind::Issues, |sender: Sender| async move {
    ///         println!("{}", sender.sender);
    ///         Ok::<_, BoxError>(())
    ///     })
    ///     .build();
    /// ```
    ///
    /// A handler that wants the meta beside the payload takes them as one
    /// input, `Event { meta, payload }: Event<P>`, not as two parameters.
    /// Written with two, `async fn notify(meta: EventMeta, pr:
    /// PullRequestNumber)`, it is refused before any message of this crate's
    /// can name the input, since rustc checks the `Fn` bound's argument count
    /// first (abridged):
    ///
    /// ```text
    /// error[E0593]: function is expected to take 1 argument, but it takes 2 arguments
    ///    |
    ///    | async fn notify(meta: EventMeta, pr: PullRequestNumber) -> Result<(), BoxError> {
    ///    | ------------------------------------------------------------------------------- takes 2 arguments
    /// ...
    ///    |     .on(Action::Opened, notify)
    ///    |                         ^^^^^^ expected function that takes 1 argument
    ///    |
    ///    = note: required for `fn(EventMeta, PullRequestNumber) -> ... {notify}` to implement `Handler<_>`
    /// ```
    ///
    /// The fix is the one parameter, destructured, as `notify` above:
    /// `async fn notify(Event { meta, payload }: Event<PullRequestNumber>)`.
    ///
    #[cfg_attr(feature = "derive", doc = "```compile_fail,E0593")]
    #[cfg_attr(not(feature = "derive"), doc = "```ignore")]
    /// use octoevents::{Action, BoxError, Dispatcher, EventKind, EventMeta};
    ///
    /// #[derive(serde::Deserialize, octoevents::Payload)]
    /// #[payload(EventKind::PullRequest)]
    /// struct PullRequestNumber { number: u64 }
    ///
    /// async fn notify(meta: EventMeta, pr: PullRequestNumber) -> Result<(), BoxError> {
    ///     println!("{}: PR #{}", meta.delivery_id, pr.number);
    ///     Ok(())
    /// }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .on(Action::Opened, notify)
    ///     .build();
    /// ```
    #[must_use]
    #[track_caller]
    pub fn on<I, H, M>(mut self, matcher: M, handler: H) -> Self
    where
        I: FromEnvelope + 'static,
        H: Handler<I> + MaybeSend + MaybeSync + 'static,
        H::Error: Into<BoxError>,
        M: IntoMatcher<I>,
    {
        let route = Route::routed(handler);
        self.insert_each(matcher.into_matcher().into_slots(), &route);
        self
    }

    /// Appends a handler over the [`Envelope`] to the chain that runs when no
    /// routed chain matched.
    ///
    /// Several may be registered; they run in order and stop at the first
    /// error. Like `always`, the chain receives the envelope and nothing is
    /// decoded on its behalf, so a strict fallback reports its own error for
    /// an unmatched payload nothing can decode, not a decode error. The chain
    /// cannot see the match: it runs alike for a kind the route table never
    /// registered and for an action GitHub added to a kind it did
    /// ([`Match::UnmatchedKind`] and [`Match::UnmatchedAction`]), and the
    /// envelope does not say which. A policy that tells the two apart, or
    /// that dead-letters an unmatched delivery without failing it, reads the
    /// [`Outcome`] from a handler wrapping the dispatcher, as
    /// [The policy seam](Dispatcher#the-policy-seam) describes.
    ///
    /// The usual fallback logs what nothing routed and leaves the delivery
    /// green in GitHub:
    ///
    /// ```
    /// use octoevents::{BoxError, Dispatcher, Envelope};
    ///
    /// async fn log_unrouted(envelope: Envelope) -> Result<(), BoxError> {
    ///     let meta = &envelope.meta;
    ///     println!("unrouted {} {} {:?}", meta.delivery_id, meta.kind, meta.action);
    ///     Ok(())
    /// }
    ///
    /// let dispatcher = Dispatcher::builder().fallback(log_unrouted).build();
    /// # let _ = dispatcher;
    /// ```
    ///
    /// A strict fallback fails the delivery instead, so an unmatched delivery
    /// shows as a failure in GitHub's delivery log rather than passing
    /// silently. Not seeing the match, it rejects an action GitHub added to a
    /// kind the route table knows as readily as a kind it does not; and with
    /// the receiver built with `handle_ping(true)`, it rejects the `ping`
    /// GitHub sends on creating the webhook unless a route registers that
    /// kind. "Log it, then reject it" is the two handlers in that order:
    ///
    /// ```
    /// use octoevents::{BoxError, Dispatcher, Envelope, EventKind};
    /// # async fn log_unrouted(_: Envelope) -> Result<(), BoxError> { Ok(()) }
    ///
    /// #[derive(Debug, thiserror::Error)]
    /// #[error("no handler for {0} events")]
    /// struct Unhandled(EventKind);
    ///
    /// async fn reject(envelope: Envelope) -> Result<(), Unhandled> {
    ///     Err(Unhandled(envelope.meta.kind))
    /// }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .fallback(log_unrouted)
    ///     .fallback(reject)
    ///     .build();
    /// # let _ = dispatcher;
    /// ```
    #[must_use]
    #[track_caller]
    pub fn fallback<H>(mut self, handler: H) -> Self
    where
        H: Handler<Envelope> + MaybeSend + MaybeSync + 'static,
        H::Error: Into<BoxError>,
    {
        self.routes.fallback.push(Route::over_envelope(handler));
        self
    }

    /// Finishes the dispatcher.
    #[must_use]
    pub fn build(self) -> Dispatcher {
        Dispatcher {
            routes: Arc::new(self.routes),
        }
    }

    /// Registers one handler under every slot; the route is shared, not
    /// duplicated.
    fn insert_each(&mut self, slots: impl IntoIterator<Item = Slot>, route: &Route) {
        for slot in slots {
            self.insert(slot, route.clone());
        }
    }

    fn insert(&mut self, slot: Slot, route: Route) {
        let routes = self.routes.by_kind.entry(slot.kind).or_default();
        let chain = match slot.action {
            Some(action) => routes.by_action.entry(action).or_default(),
            None => &mut routes.any_action,
        };
        chain.push(route);
    }
}

/// One registered handler, its name, and where it was registered.
///
/// A handler registered under several slots is one route cloned per slot:
/// the erased handler is shared, and every clone points at the same
/// registration.
///
/// The two constructors take the handler itself, not an erased one, so the
/// handler name is always that of the handler erased: neither can be
/// recorded without the other.
struct Route {
    handler: Arc<dyn ErasedHandler>,
    /// [`type_name`] of the handler before erasure: a static string, on
    /// `wasm32` as anywhere.
    handler_name: &'static str,
    /// The call to the registration method, captured through
    /// `#[track_caller]`: a static reference, on `wasm32` as anywhere.
    registration_site: &'static Location<'static>,
}

impl Route {
    /// A routed handler, erased behind its input's decode: the route decodes
    /// `I` from the envelope when it runs, so a route that never matches
    /// never decodes, and a decode failure is this route's failure.
    ///
    /// `#[track_caller]` here, on [`registered`](Self::registered) below and
    /// on the registration method calling this makes the location the
    /// consumer's call to that method, not any frame of this chain.
    #[track_caller]
    fn routed<I, H>(handler: H) -> Self
    where
        I: FromEnvelope + 'static,
        H: Handler<I> + MaybeSend + MaybeSync + 'static,
        H::Error: Into<BoxError>,
    {
        Self::registered(
            Arc::new(Routed {
                handler,
                input: PhantomData,
            }),
            type_name::<H>(),
        )
    }

    /// A handler over the envelope for the `always` and `fallback` tiers,
    /// erased as [`OverEnvelope`].
    ///
    /// `#[track_caller]` as on [`routed`](Self::routed).
    #[track_caller]
    fn over_envelope<H>(handler: H) -> Self
    where
        H: Handler<Envelope> + MaybeSend + MaybeSync + 'static,
        H::Error: Into<BoxError>,
    {
        Self::registered(Arc::new(OverEnvelope { handler }), type_name::<H>())
    }

    /// Pairs an erased handler and its name with the location
    /// `#[track_caller]` resolves to: the consumer's call to the registration
    /// method, through the constructor above and that method.
    #[track_caller]
    fn registered(handler: Arc<dyn ErasedHandler>, handler_name: &'static str) -> Self {
        Self {
            handler,
            handler_name,
            registration_site: Location::caller(),
        }
    }

    /// Wraps this route's failure with the tier it ran in, the delivery, and
    /// the handler name and registration site the route carries.
    fn failed(&self, tier: Tier, envelope: &Envelope, source: BoxError) -> DispatchError {
        let meta = &envelope.meta;
        DispatchError {
            tier,
            handler: self.handler_name,
            registration_site: self.registration_site,
            delivery_id: meta.delivery_id.clone(),
            kind: meta.kind.clone(),
            action: meta.action.clone(),
            source,
        }
    }
}

impl Clone for Route {
    fn clone(&self) -> Self {
        Self {
            handler: Arc::clone(&self.handler),
            handler_name: self.handler_name,
            registration_site: self.registration_site,
        }
    }
}

// The erased handler is never `Debug`; its name and where it was registered
// are what an operator reading the route table wants, so a route prints as
// `Route(app::revoke, src/main.rs:12:10, ..)`, the `..` standing for the
// elided handler as in `WebhookReceiver`'s `Debug`.
impl fmt::Debug for Route {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Route")
            .field(&format_args!("{}", self.handler_name))
            .field(&format_args!("{}", self.registration_site))
            .finish_non_exhaustive()
    }
}

/// Every chain a dispatcher can run.
struct Routes {
    always: Vec<Route>,
    by_kind: HashMap<EventKind, KindRoutes>,
    fallback: Vec<Route>,
}

impl Routes {
    /// Looks one delivery up in the route table: the match it decides and the
    /// routed chains it selects, the action-specific chain before the
    /// kind-wide one.
    ///
    /// This is the whole of matching: the tiers that run afterwards cannot
    /// change it. Routes are keyed by kind first so the lookup is entirely by
    /// reference: no `EventKind` or `Action` is cloned to build a key.
    fn lookup(&self, meta: &EventMeta) -> (Match, impl Iterator<Item = &[Route]>) {
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
#[derive(Default)]
struct KindRoutes {
    any_action: Vec<Route>,
    by_action: HashMap<Action, Vec<Route>>,
}

impl fmt::Debug for KindRoutes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KindRoutes")
            .field("any_action", &self.any_action)
            .field("by_action", &self.by_action)
            .finish()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
