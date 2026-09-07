use std::{future::Future, sync::Arc};

use crate::{MaybeSend, MaybeSync};

/// Consumer-owned code that handles one delivery, received as `I`.
///
/// One trait, named by what it receives. `I` is any
/// [`FromEnvelope`](crate::FromEnvelope), and the input type says what the
/// handler gets and what is decoded for it:
///
/// | `I`                                   | receives                                  |
/// |---------------------------------------|-------------------------------------------|
/// | [`Envelope`](crate::Envelope)         | the meta and the exact payload bytes      |
/// | [`EventMeta`](crate::EventMeta)       | the meta alone; nothing is decoded        |
/// | `P: `[`Payload`](crate::Payload)      | the payload decoded as `P`, kind checked  |
/// | [`Event<P>`](crate::Event)            | the meta beside the payload decoded as `P`|
///
/// The receiver and the dispatcher's `always` and `fallback` tiers take a
/// `Handler<Envelope>`; the dispatcher's routes take a handler over any of
/// them. The simplest handler is an `async fn` taking its input and returning
/// `Result<(), E>`; the receiver and the dispatcher accept the function
/// itself, and the parameter's type is what fixes `I`:
///
/// ```
/// use octoevents::{Envelope, Event, EventKind, EventMeta, Handler};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// // Bytes included: what the receiver and the `always` tier take.
/// async fn audit(envelope: Envelope) -> Result<(), std::io::Error> {
///     println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw_payload.len());
///     Ok(())
/// }
///
/// // Meta only: routed by kind and action, nothing decoded.
/// async fn revoke(meta: EventMeta) -> Result<(), std::io::Error> {
///     println!("revoke tokens for installation {:?}", meta.installation_id);
///     Ok(())
/// }
///
/// // Payload only: the kind comes from the type.
/// async fn label(pr: PullRequestNumber) -> Result<(), std::io::Error> {
///     println!("label PR #{}", pr.number);
///     Ok(())
/// }
///
/// // Meta and payload, destructured in the parameter.
/// async fn notify(Event { meta, payload }: Event<PullRequestNumber>) -> Result<(), std::io::Error> {
///     println!("{}: PR #{}", meta.delivery_id, payload.number);
///     Ok(())
/// }
/// # fn assert_handler<I, H: Handler<I>>(_: H) {}
/// # assert_handler(audit);
/// # assert_handler(revoke);
/// # assert_handler(label);
/// # assert_handler(notify);
/// ```
///
/// A handler with dependencies is a struct implementing the trait, the
/// dependencies its fields, with a plain `async fn handle`; the future
/// borrows `&self`, so nothing is cloned per delivery:
///
/// ```
/// use octoevents::{Event, EventKind, Handler};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// struct Labeler {
///     label: String, // stands in for a GitHub API client
/// }
///
/// impl Handler<Event<PullRequestNumber>> for Labeler {
///     type Error = std::io::Error;
///
///     async fn handle(&self, Event { meta, payload }: Event<PullRequestNumber>) -> Result<(), Self::Error> {
///         println!("{}: label PR #{} {}", meta.delivery_id, payload.number, self.label);
///         Ok(())
///     }
/// }
/// ```
///
/// Closures implement the trait too, with two annotations the `async fn`
/// form does not need: name the parameter's type (`|envelope: Envelope|`)
/// and state the error type (`Ok::<_, E>(())`). Registration is bound on the
/// handler trait rather than on `Fn`, so rustc reads neither off the call: a
/// bare `Ok(())` is E0282 on the receiver path, where nothing constrains it,
/// and E0283 on the dispatcher path, where every error type the application
/// error has a `From` for would fit.
///
/// ```
/// use octoevents::{Envelope, Handler};
///
/// fn log() -> impl Handler<Envelope, Error = std::io::Error> {
///     |envelope: Envelope| async move {
///         println!("{} {}", envelope.meta.delivery_id, envelope.meta.kind);
///         Ok::<_, std::io::Error>(())
///     }
/// }
/// ```
///
/// The trait is generic over the input, so one struct can implement it for
/// several. Registration then needs a turbofish, because the struct alone no
/// longer says which input is meant:
/// `dispatcher.on_payload::<PullRequestNumber, _>(labeler)`. An `async fn` or
/// a closure fixes the input by its parameter type and needs none.
///
/// `Arc<H>` is a handler when `H` is, so one handler struct can be shared
/// between the receiver or a route and a test that reads its state. `&H` and
/// `Box<H>` are not: std implements `Fn` for both, so those impls would
/// overlap the closure blanket. Neither the receiver nor the dispatcher needs
/// the caller's `Arc`: each holds its handlers behind its own, so the
/// receiver is `Clone` for any `H` without one.
///
/// For one kind and nothing else, no dispatcher is needed: a handler over the
/// envelope decodes its own view with
/// [`Envelope::decode_payload`](crate::Envelope::decode_payload), whose kind
/// check refuses a delivery of another kind at the kind, so a misconfigured
/// webhook fails loudly rather than at a missing field. The crate docs show
/// one under [One event, one handler](crate#one-event-one-handler).
///
/// Passing something that is not a handler names the input and the shape
/// expected rather than the `Fn` bound behind it. For a struct with no impl,
/// rustc reports (abridged):
///
/// ```text
/// error[E0277]: `Persist` is not a handler over `Envelope`
///   |
///   |     assert_handler(Persist);
///   |                    ^^^^^^^ expected an `impl Handler<Envelope>` or a one-argument closure `|input: Envelope| async { .. }`
///   |
///   = note: a handler receives exactly one input, decoded from the envelope: `Envelope` (bytes included), `EventMeta` (meta only), a `Payload` view `P` (payload only), `Event<P>` (meta and payload), or a consumer type implementing `FromEnvelope` itself
///   = note: implement `Handler<Envelope>` with `async fn handle(&self, input: Envelope) -> Result<(), Self::Error>`
/// ```
///
/// ```compile_fail,E0277
/// use octoevents::{Envelope, Handler};
///
/// fn assert_handler<H: Handler<Envelope>>(_: H) {}
///
/// struct Persist;
/// assert_handler(Persist);
/// ```
///
/// A function of the wrong arity is the one shape that message cannot reach:
/// rustc checks the `Fn` bound's argument count before any trait message
/// fires, so `async fn notify(meta: EventMeta, pr: PullRequestNumber)` passed
/// to a registration method is E0593, "function is expected to take 1
/// argument, but it takes 2 arguments". Meta and payload together is one
/// input, destructured in the parameter: `Event { meta, payload }: Event<P>`.
/// [`DispatcherBuilder::on`](crate::DispatcherBuilder::on) shows the error
/// and the fix.
///
/// # `MaybeSync` and `MaybeSend`
///
/// The trait carries two platform-conditional bounds: a handler is
/// [`MaybeSync`], the supertrait, and the future `handle` returns is
/// [`MaybeSend`].
///
/// On native targets they are `Sync` and `Send`. A server polls a delivery's
/// future from any of its threads, so the handler is shared between threads
/// and its future moves between them.
///
/// On `wasm32`, where a Cloudflare Worker runs on one JavaScript event loop,
/// both bounds are empty: a handler may hold `Rc` state or JavaScript values,
/// and its future may await a JavaScript promise.
///
/// Both bounds are on the trait. The future's, because `async fn` in a trait
/// cannot name an auto-trait bound, and implementors still write `async fn`;
/// the handler's, so that it is stated once, where every handler already
/// satisfies it to be registered.
///
/// The supertrait is what lets an adapter generic over another handler be
/// written with no bound on `H` beyond the trait. Its `async fn handle` holds
/// `&self.inner` across the await, so its future is `Send` only if `H` is
/// `Sync`, and `H: Handler<I>` says so; without the supertrait rustc would
/// suggest `H: Sync`, which compiles natively and refuses a single-threaded
/// handler on `wasm32`.
///
/// The input is a separate matter. An `async fn`'s future owns its arguments
/// from creation, so an adapter generic over the payload bounds it
/// `P: MaybeSend`, the platform-conditional spelling of the `P: Send` rustc
/// suggests. An adapter that calls `self.inner.handle(event)` before its own
/// `async move` block captures neither the input nor `&self` and needs no
/// bound at all.
///
/// ```
/// use octoevents::{Event, Handler, MaybeSend};
///
/// /// Forwards to `inner` once it has read the meta.
/// struct Audited<H> {
///     inner: H,
/// }
///
/// impl<P: MaybeSend, H: Handler<Event<P>>> Handler<Event<P>> for Audited<H> {
///     type Error = H::Error;
///
///     async fn handle(&self, event: Event<P>) -> Result<(), Self::Error> {
///         let delivery_id = event.meta.delivery_id.clone();
///         let result = self.inner.handle(event).await;
///         if result.is_err() {
///             eprintln!("{delivery_id} failed");
///         }
///         result
///     }
/// }
/// ```
///
/// The registration methods bound a handler `MaybeSend + MaybeSync +
/// 'static`, repeating the `MaybeSync` the trait implies, for the
/// diagnostic's sake. A closure capturing `!Sync` state its future never
/// touches, a `Cell` read before the future is built, fails the closure
/// blanket; the blanket is hidden from rustc's explanation so that the
/// trait's message can name the input, and on the trait bound alone the
/// closure is reported as not a handler. The repeated bound is what rustc
/// reports instead (abridged):
///
/// ```text
/// error[E0277]: `Cell<u32>` cannot be shared between threads safely
///   |
///   |     .always(move |_: Envelope| {
///   |      ------ ^----------------- `Cell<u32>` cannot be shared between threads safely
///   |
///   = help: within `{closure}`, the trait `Sync` is not implemented for `Cell<u32>`
///   = note: required for `{closure}` to implement `MaybeSync`
/// note: required by a bound in `DispatcherBuilder::<E>::always`
/// ```
///
/// A struct holding such state never reaches registration: its `impl` is
/// refused at the supertrait, naming the field's type.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a handler over `{I}`",
    label = "expected an `impl Handler<{I}>` or a one-argument closure `|input: {I}| async {{ .. }}`",
    note = "a handler receives exactly one input, decoded from the envelope: `Envelope` (bytes included), `EventMeta` (meta only), a `Payload` view `P` (payload only), `Event<P>` (meta and payload), or a consumer type implementing `FromEnvelope` itself",
    note = "implement `Handler<{I}>` with `async fn handle(&self, input: {I}) -> Result<(), Self::Error>`"
)]
pub trait Handler<I>: MaybeSync {
    /// The error this handler reports for a failed delivery.
    type Error;

    /// Handles one delivery, received as `I`.
    fn handle(&self, input: I) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;
}

// The closure blanket returns the closure's future directly. Wrapping it in
// an `async fn` would capture `&self` and the input across the await and
// demand a `Send` input for no benefit.
//
// `do_not_recommend` keeps rustc from explaining a missing impl as "the trait
// `Fn(I)` is not implemented": the trait's own `on_unimplemented` message
// names the input and the shape instead.
#[diagnostic::do_not_recommend]
impl<I, F, Fut, E> Handler<I> for F
where
    F: Fn(I) -> Fut + MaybeSync,
    Fut: Future<Output = Result<(), E>> + MaybeSend,
{
    type Error = E;

    #[allow(refining_impl_trait)]
    fn handle(&self, input: I) -> Fut {
        self(input)
    }
}

// `Arc<H>` does not overlap the closure blanket: `Fn` is a fundamental trait
// and std implements it for `&F` and `Box<F>` but not for `Arc<F>`, so rustc
// knows `Arc<H>: Fn(..)` never holds. The same reasoning is why `&H` and
// `Box<H>` cannot be handlers.
//
// `Arc<H>` is `Sync` only when `H` is `Send` as well as `Sync`; the trait
// supplies `MaybeSync`, and `MaybeSend` is what registration requires of `H`
// anyway.
impl<I, H: Handler<I> + MaybeSend> Handler<I> for Arc<H> {
    type Error = H::Error;

    fn handle(&self, input: I) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        H::handle(self, input)
    }
}
