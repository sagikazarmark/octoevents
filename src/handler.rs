use std::future::Future;

#[cfg(feature = "octocrab")]
use octocrab::models::webhook_events::WebhookEvent;

use crate::{Envelope, EventMeta, MaybeSend, Payload};

/// Consumer-owned code that handles one verified [`Envelope`].
///
/// This is the handler flavour the receiver accepts, and the one the
/// dispatcher's `always` and `fallback` tiers accept. Implement it on a
/// struct whose fields are its dependencies and write a plain
/// `async fn handle`; the future borrows `&self`, so nothing is cloned per
/// delivery:
///
/// ```
/// use octoevents::{Envelope, WebhookHandler};
///
/// struct Persist {
///     store: std::sync::Arc<Vec<u8>>, // stands in for a database pool
/// }
///
/// impl WebhookHandler for Persist {
///     type Error = std::io::Error;
///
///     async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
///         let _ = (&self.store, envelope.raw.len());
///         Ok(())
///     }
/// }
/// ```
///
/// Closures implement the trait too. Annotate the parameter type and state
/// the error type (`Ok::<_, E>(())`): neither the receiver nor a dispatcher
/// can infer it from a bare `Ok(())`.
///
/// ```
/// use octoevents::{Envelope, WebhookHandler};
///
/// fn log() -> impl WebhookHandler<Error = std::convert::Infallible> {
///     |envelope: Envelope| async move {
///         println!("{} {}", envelope.meta.delivery_id, envelope.meta.kind);
///         Ok::<_, std::convert::Infallible>(())
///     }
/// }
/// ```
///
/// The future must be `Send` on native targets and is unconstrained on
/// `wasm32`, which is what [`MaybeSend`] spells. The bound is stated on the
/// trait because `async fn` in a trait cannot name an auto-trait bound;
/// implementors still write `async fn`.
///
/// For one kind and nothing else, no dispatcher is needed: a webhook handler
/// decodes its own view with [`Envelope::decode_payload`], whose kind check
/// refuses a delivery of another kind at the kind, so a misconfigured webhook
/// fails loudly rather than at a missing field. The handler's error type
/// absorbs the [`DecodeError`](crate::DecodeError) through `From`, here by
/// being it:
///
/// ```
/// use octoevents::{DecodeError, Envelope, EventKind, WebhookHandler};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// struct Labeler { /* GitHub API client */ }
///
/// impl WebhookHandler for Labeler {
///     type Error = DecodeError;
///
///     async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
///         let pr = envelope.decode_payload::<PullRequestNumber>()?;
///         println!("{}: label PR #{}", envelope.meta.delivery_id, pr.number);
///         Ok(())
///     }
/// }
/// ```
///
/// The alternative is a `Dispatcher` with one route (`on_payload`, or
/// `on_payload_action` for some of the kind's actions), which takes a
/// [`PayloadHandler`] over the view and answers a delivery of any other kind
/// with success rather than failure.
///
/// `&H` and `Box<H>` are not webhook handlers when `H` is: std implements
/// `Fn` for both, so those impls would overlap the closure blanket. The
/// receiver holds its handler behind its own `Arc`, so it is `Clone` for any
/// `H` without one.
///
/// Passing something that is not a handler names the flavour and its shape
/// rather than the `Fn` bound behind it. For a struct with no impl, rustc
/// reports (abridged):
///
/// ```text
/// error[E0277]: `Persist` is not a webhook handler
///   |
///   |     assert_handler(Persist);
///   |                    ^^^^^^^ expected an `impl WebhookHandler` or a closure `|envelope: Envelope| async { .. }`
///   |
///   = note: a webhook handler receives the verified `Envelope`: implement `WebhookHandler` with `async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error>`
/// ```
///
/// ```compile_fail,E0277
/// use octoevents::WebhookHandler;
///
/// fn assert_handler<H: WebhookHandler>(_: H) {}
///
/// struct Persist;
/// assert_handler(Persist);
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a webhook handler",
    label = "expected an `impl WebhookHandler` or a closure `|envelope: Envelope| async {{ .. }}`",
    note = "a webhook handler receives the verified `Envelope`: implement `WebhookHandler` with `async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error>`"
)]
pub trait WebhookHandler {
    /// The error this handler reports for a failed delivery.
    type Error;

    /// Handles one verified envelope.
    fn handle(
        &self,
        envelope: Envelope,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;
}

// Each closure blanket (here and on the two other flavours below) returns
// the closure's future directly. Wrapping it in an `async fn` would capture
// `&self` and the arguments across the await and demand `F: Sync` and
// `Send` arguments of the closure for no benefit.
//
// `do_not_recommend` keeps rustc from explaining a missing impl as "the trait
// `Fn(Envelope)` is not implemented": the trait's own `on_unimplemented`
// message names the flavour instead.
#[diagnostic::do_not_recommend]
impl<F, Fut, E> WebhookHandler for F
where
    F: Fn(Envelope) -> Fut,
    Fut: Future<Output = Result<(), E>> + MaybeSend,
{
    type Error = E;

    #[allow(refining_impl_trait)]
    fn handle(&self, envelope: Envelope) -> Fut {
        self(envelope)
    }
}

/// Consumer-owned code that handles octocrab's decoded [`WebhookEvent`] for
/// any kind.
///
/// The handler receives the [`EventMeta`] and octocrab's `WebhookEvent`,
/// whose `specific` payload is an enum over every kind octocrab models. This
/// is the flavour for logic that spans kinds (an auditor, a metrics counter,
/// moderation across `issues` and `issue_comment`); for one kind's payload,
/// prefer a [`PayloadHandler`], which needs no `match`.
///
/// ```
/// use octocrab::models::webhook_events::WebhookEvent;
/// use octoevents::{EventHandler, EventMeta};
///
/// struct Auditor { /* database pool */ }
///
/// impl EventHandler for Auditor {
///     type Error = std::io::Error;
///
///     async fn handle(&self, meta: EventMeta, event: WebhookEvent) -> Result<(), Self::Error> {
///         println!("{} from {:?}", meta.delivery_id, event.sender.map(|sender| sender.login));
///         Ok(())
///     }
/// }
/// ```
///
/// A closure `Fn(EventMeta, WebhookEvent) -> Fut` is an event handler too;
/// annotate the parameters it uses and state its error type
/// (`Ok::<_, E>(())`):
///
/// ```
/// use octocrab::models::webhook_events::WebhookEvent;
/// use octoevents::{EventHandler, EventMeta};
///
/// fn audit() -> impl EventHandler<Error = std::convert::Infallible> {
///     |meta: EventMeta, event: WebhookEvent| async move {
///         println!("{} {:?}", meta.delivery_id, event.kind);
///         Ok::<_, std::convert::Infallible>(())
///     }
/// }
/// ```
///
/// Enabling the `octocrab` feature makes octocrab's pre-1.0 version part of
/// this crate's public API: `WebhookEvent` is octocrab's type, so an octocrab
/// major bump here is a breaking change for this trait and for
/// `Dispatcher::on`, the one dispatcher method that accepts it.
///
/// `&H` and `Box<H>` are not event handlers when `H` is: std implements `Fn`
/// for both, so those impls would overlap the closure blanket. The dispatcher
/// wraps every registered handler in its own `Arc`, so sharing needs nothing
/// from the caller.
///
/// A value that is not an event handler is reported as such, with the shape
/// expected:
///
/// ```compile_fail,E0277
/// use octoevents::EventHandler;
///
/// fn assert_handler<H: EventHandler>(_: H) {}
///
/// struct Auditor;
/// assert_handler(Auditor);
/// ```
///
/// [`WebhookEvent`]: octocrab::models::webhook_events::WebhookEvent
#[cfg(feature = "octocrab")]
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not an event handler",
    label = "expected an `impl EventHandler` or a closure `|meta: EventMeta, event: WebhookEvent| async {{ .. }}`",
    note = "an event handler receives the `EventMeta` and octocrab's decoded `WebhookEvent`: implement `EventHandler` with `async fn handle(&self, meta: EventMeta, event: WebhookEvent) -> Result<(), Self::Error>`"
)]
pub trait EventHandler {
    /// The error this handler reports for a failed delivery.
    type Error;

    /// Handles one delivery decoded as octocrab's `WebhookEvent`.
    fn handle(
        &self,
        meta: EventMeta,
        event: WebhookEvent,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;
}

#[cfg(feature = "octocrab")]
#[diagnostic::do_not_recommend]
impl<F, Fut, E> EventHandler for F
where
    F: Fn(EventMeta, WebhookEvent) -> Fut,
    Fut: Future<Output = Result<(), E>> + MaybeSend,
{
    type Error = E;

    #[allow(refining_impl_trait)]
    fn handle(&self, meta: EventMeta, event: WebhookEvent) -> Fut {
        self(meta, event)
    }
}

/// Consumer-owned code that handles one kind's decoded payload.
///
/// The kind is declared by the payload type: `P::KIND` is the only kind whose
/// deliveries reach `handle`, so a handler over
/// `PullRequestWebhookEventPayload` cannot be registered under `issues`. The
/// handler receives the [`EventMeta`] for the delivery ID and installation ID
/// and the decoded payload; it does not receive the raw bytes, so a decoded
/// handler has one source of truth.
///
/// ```
/// use octoevents::{EventKind, EventMeta, PayloadHandler};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// struct Labeler { /* GitHub API client */ }
///
/// impl PayloadHandler<PullRequestNumber> for Labeler {
///     type Error = std::io::Error;
///
///     async fn handle(&self, meta: EventMeta, pr: PullRequestNumber) -> Result<(), Self::Error> {
///         println!("{}: label PR #{}", meta.delivery_id, pr.number);
///         Ok(())
///     }
/// }
/// ```
///
/// A closure `Fn(EventMeta, P) -> Fut` is a payload handler too. Annotate
/// the parameters it uses (the payload type is also what fixes `P`) and
/// state its error type (`Ok::<_, E>(())`):
///
/// ```
/// use octoevents::{EventKind, EventMeta, PayloadHandler};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// fn log() -> impl PayloadHandler<PullRequestNumber, Error = std::convert::Infallible> {
///     |meta: EventMeta, pr: PullRequestNumber| async move {
///         println!("{}: PR #{}", meta.delivery_id, pr.number);
///         Ok::<_, std::convert::Infallible>(())
///     }
/// }
/// ```
///
/// Register one on a `Dispatcher` with `on_payload` for every action of its
/// kind or `on_payload_action` for some. The dispatcher decodes with
/// [`Envelope::decode`] only once a route has matched, so the kind check is
/// the route table's. For one kind and nothing else, a [`WebhookHandler`]
/// calling [`Envelope::decode_payload`] needs no dispatcher.
///
/// The trait is generic over the payload, so one struct can implement it for
/// several payload types. Registration then needs a turbofish, because the
/// struct alone no longer says which payload is meant:
/// `dispatcher.on_payload::<PullRequestNumber, _>(labeler)`. A closure fixes
/// the payload by its parameter type and needs no turbofish.
///
/// `&H` and `Box<H>` are not payload handlers when `H` is: std implements
/// `Fn` for both, so those impls would overlap the closure blanket. The
/// dispatcher wraps every registered handler in its own `Arc`, so sharing
/// needs nothing from the caller.
///
/// A value that is not a payload handler is reported as such, naming the
/// payload and the shape expected:
///
/// ```compile_fail,E0277
/// use octoevents::{EventKind, PayloadHandler};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// fn assert_handler<H: PayloadHandler<PullRequestNumber>>(_: H) {}
///
/// struct Labeler;
/// assert_handler(Labeler);
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a payload handler for `{P}`",
    label = "expected an `impl PayloadHandler<{P}>` or a closure `|meta: EventMeta, payload: {P}| async {{ .. }}`",
    note = "a payload handler receives the `EventMeta` and one kind's decoded payload: implement `PayloadHandler<{P}>` with `async fn handle(&self, meta: EventMeta, payload: {P}) -> Result<(), Self::Error>`"
)]
pub trait PayloadHandler<P: Payload> {
    /// The error this handler reports for a failed delivery.
    type Error;

    /// Handles one delivery whose payload decoded as `P`.
    fn handle(
        &self,
        meta: EventMeta,
        payload: P,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;
}

#[diagnostic::do_not_recommend]
impl<P, F, Fut, E> PayloadHandler<P> for F
where
    P: Payload,
    F: Fn(EventMeta, P) -> Fut,
    Fut: Future<Output = Result<(), E>> + MaybeSend,
{
    type Error = E;

    #[allow(refining_impl_trait)]
    fn handle(&self, meta: EventMeta, payload: P) -> Fut {
        self(meta, payload)
    }
}
