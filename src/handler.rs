use std::{future::Future, sync::Arc};

use crate::{Envelope, EventMeta, MaybeSend};

/// Consumer-owned code that handles one verified [`Envelope`].
///
/// This is the handler flavour the receiver accepts, and the one the
/// dispatcher's `always` and `fallback` tiers accept. It receives the
/// envelope as verified: the [`EventMeta`] and the exact payload bytes, with
/// nothing decoded on its behalf, so it runs for every delivery, including
/// one whose payload nothing can decode.
///
/// The simplest handler is an `async fn` taking the envelope and returning
/// `Result<(), E>`; the receiver and the dispatcher accept the function
/// itself:
///
/// ```
/// use octoevents::{Envelope, WebhookHandler};
///
/// async fn audit(envelope: Envelope) -> Result<(), std::io::Error> {
///     println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw.len());
///     Ok(())
/// }
/// # fn assert_handler<H: WebhookHandler>(_: H) {}
/// # assert_handler(audit);
/// ```
///
/// A handler with dependencies is a struct implementing the trait, the
/// dependencies its fields, with a plain `async fn handle`; the future
/// borrows `&self`, so nothing is cloned per delivery:
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
/// Closures implement the trait too, with two annotations the `async fn`
/// form does not need: name the type of each parameter the body uses
/// (`|envelope: Envelope|`; one the body ignores may stay `_`) and state the
/// error type (`Ok::<_, E>(())`). Registration is bound on the handler
/// trait rather than on `Fn`, so rustc reads neither off the call: a bare
/// `Ok(())` is E0282 on the receiver path, where nothing constrains it, and
/// E0283 on the dispatcher path, where every error type the application
/// error has a `From` for would fit. This holds for [`EventHandler`]
/// closures alike.
///
/// ```
/// use octoevents::{Envelope, WebhookHandler};
///
/// fn log() -> impl WebhookHandler<Error = std::io::Error> {
///     |envelope: Envelope| async move {
///         println!("{} {}", envelope.meta.delivery_id, envelope.meta.kind);
///         Ok::<_, std::io::Error>(())
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
/// fails loudly rather than at a missing field. The crate docs show one under
/// [One event, one webhook handler](crate#one-event-one-webhook-handler). The
/// alternative is a `Dispatcher` with one route (`on_payload`, or
/// `on_payload_action` for some of the kind's actions), which takes an
/// [`EventHandler`] over the view and answers a delivery of any other kind
/// with success rather than failure.
///
/// `Arc<H>` is a handler of either flavour when `H` is, so one handler
/// struct can be shared between the receiver or a route and a test that
/// reads its state. `&H` and `Box<H>` are not: std implements `Fn` for both,
/// so those impls would overlap the closure blanket. Neither the receiver
/// nor the dispatcher needs the caller's `Arc`: each holds its handlers
/// behind its own, so the receiver is `Clone` for any `H` without one.
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

// Each closure blanket (here and on the event handler below) returns
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

// `Arc<H>` does not overlap the closure blanket: `Fn` is a fundamental trait
// and std implements it for `&F` and `Box<F>` but not for `Arc<F>`, so rustc
// knows `Arc<H>: Fn(..)` never holds. The same reasoning is why `&H` and
// `Box<H>` cannot be handlers.
impl<H: WebhookHandler> WebhookHandler for Arc<H> {
    type Error = H::Error;

    fn handle(
        &self,
        envelope: Envelope,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        H::handle(self, envelope)
    }
}

/// Consumer-owned code that handles one envelope decoded as `P`.
///
/// The handler receives the [`EventMeta`] for the delivery ID, kind, action
/// and installation ID, and the envelope decoded as `P`; it does not receive
/// the raw bytes, so a decoded handler has one source of truth. `P` is any
/// [`FromEnvelope`](crate::FromEnvelope): a [`Payload`](crate::Payload) view
/// over one kind, `()` for a handler that needs only the meta, octocrab's
/// `WebhookEvent` with the `octocrab` feature, or a consumer type that
/// implements `FromEnvelope` itself for a view over several kinds.
///
/// A handler over a `Payload` is bound to the kind the payload declares:
/// `P::KIND` is the only kind whose deliveries reach `handle`, so a handler
/// over `PullRequestWebhookEventPayload` cannot be registered under `issues`.
///
/// The simplest handler is an `async fn` taking the meta and the input and
/// returning `Result<(), E>`; the dispatcher accepts the function itself, and
/// the input's type is what fixes `P`:
///
/// ```
/// use octoevents::{EventHandler, EventKind, EventMeta};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// async fn log(meta: EventMeta, pr: PullRequestNumber) -> Result<(), std::io::Error> {
///     println!("{}: PR #{}", meta.delivery_id, pr.number);
///     Ok(())
/// }
///
/// // Routed by kind and action alone; nothing is decoded for it.
/// async fn revoke(meta: EventMeta, (): ()) -> Result<(), std::io::Error> {
///     println!("revoke tokens for installation {:?}", meta.installation_id);
///     Ok(())
/// }
/// # fn assert_handler<P, H: EventHandler<P>>(_: H) {}
/// # assert_handler(log);
/// # assert_handler(revoke);
/// ```
///
/// A handler with dependencies is a struct implementing the trait, the
/// dependencies its fields borrowed through `&self`:
///
/// ```
/// use octoevents::{EventHandler, EventKind, EventMeta};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// struct Labeler { /* GitHub API client */ }
///
/// impl EventHandler<PullRequestNumber> for Labeler {
///     type Error = std::io::Error;
///
///     async fn handle(&self, meta: EventMeta, pr: PullRequestNumber) -> Result<(), Self::Error> {
///         println!("{}: label PR #{}", meta.delivery_id, pr.number);
///         Ok(())
///     }
/// }
/// ```
///
/// A closure `Fn(EventMeta, P) -> Fut` is an event handler too, with the
/// annotations [`WebhookHandler`] describes: the parameters the body uses
/// typed (`|meta: EventMeta, pr: PullRequestNumber|`) and the error type
/// stated.
///
/// Register one on a `Dispatcher` with `on_payload` for every action of a
/// payload's kind, `on_payload_action` for some, or `on` with a matcher for
/// any other input. The dispatcher decodes `P` only once a route has matched,
/// and a decode failure fails the delivery at that registration. For one kind
/// and nothing else, a [`WebhookHandler`] calling [`Envelope::decode_payload`]
/// needs no dispatcher.
///
/// The trait is generic over the input, so one struct can implement it for
/// several input types. Registration then needs a turbofish, because the
/// struct alone no longer says which input is meant:
/// `dispatcher.on_payload::<PullRequestNumber, _>(labeler)`. An `async fn`
/// or a closure fixes the input by its parameter type and needs no turbofish.
///
/// `Arc<H>` is an event handler when `H` is, under the rule [`WebhookHandler`]
/// states for both flavours; the dispatcher wraps every registered handler in
/// its own `Arc`, so sharing needs nothing from the caller.
///
/// A value that is not an event handler is reported as such, naming the
/// input and the shape expected:
///
/// ```compile_fail,E0277
/// use octoevents::{EventHandler, EventKind};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// fn assert_handler<H: EventHandler<PullRequestNumber>>(_: H) {}
///
/// struct Labeler;
/// assert_handler(Labeler);
/// ```
///
/// The trait places no bound on `P`; the registration methods do, and that
/// is where a closure over a serde type that has not declared its kind is
/// reported. `on_payload` reports it as not a [`Payload`](crate::Payload),
/// with the `impl_payload!` call that makes it one (abridged):
///
/// ```text
/// error[E0277]: `PullRequestNumber` is not a payload
///    |
///    |         .on_payload(|_: EventMeta, pr: PullRequestNumber| async move {
///    |          ^^^^^^^^^^ expected a `serde::Deserialize` type that declares the event kind it decodes
///    |
///    = note: declare the kind with `octoevents::impl_payload!(PullRequestNumber => EventKind::..)`
/// ```
///
/// ```compile_fail,E0277
/// use octoevents::{Dispatcher, EventMeta};
/// # use octoevents::DecodeError;
/// # struct AppError;
/// # impl From<DecodeError> for AppError { fn from(_: DecodeError) -> Self { Self } }
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
///
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .on_payload(|_: EventMeta, pr: PullRequestNumber| async move {
///         println!("PR #{}", pr.number);
///         Ok::<_, AppError>(())
///     })
///     .build();
/// ```
///
/// `on` reports the same type as not a `FromEnvelope`, naming both
/// `impl_payload!` and a direct `FromEnvelope` impl for a view over several
/// kinds.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not an event handler for `{P}`",
    label = "expected an `impl EventHandler<{P}>` or a closure `|meta: EventMeta, payload: {P}| async {{ .. }}`",
    note = "an event handler receives the `EventMeta` and the envelope decoded as `{P}`: implement `EventHandler<{P}>` with `async fn handle(&self, meta: EventMeta, payload: {P}) -> Result<(), Self::Error>`"
)]
pub trait EventHandler<P> {
    /// The error this handler reports for a failed delivery.
    type Error;

    /// Handles one envelope decoded as `P`.
    fn handle(
        &self,
        meta: EventMeta,
        payload: P,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;
}

#[diagnostic::do_not_recommend]
impl<P, F, Fut, E> EventHandler<P> for F
where
    F: Fn(EventMeta, P) -> Fut,
    Fut: Future<Output = Result<(), E>> + MaybeSend,
{
    type Error = E;

    #[allow(refining_impl_trait)]
    fn handle(&self, meta: EventMeta, payload: P) -> Fut {
        self(meta, payload)
    }
}

impl<P, H: EventHandler<P>> EventHandler<P> for Arc<H> {
    type Error = H::Error;

    fn handle(
        &self,
        meta: EventMeta,
        payload: P,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        H::handle(self, meta, payload)
    }
}
