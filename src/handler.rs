use std::{future::Future, sync::Arc};

use crate::MaybeSend;

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
///     println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw.len());
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
/// The future must be `Send` on native targets and is unconstrained on
/// `wasm32`, which is what [`MaybeSend`] spells. The bound is stated on the
/// trait because `async fn` in a trait cannot name an auto-trait bound;
/// implementors still write `async fn`.
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
/// to a registration method is E0593, "expected to take 1 argument, but it
/// takes 2". Meta and payload together is one input, `Event<P>`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a handler over `{I}`",
    label = "expected an `impl Handler<{I}>` or a one-argument closure `|input: {I}| async {{ .. }}`",
    note = "a handler receives exactly one input, decoded from the envelope: `Envelope` (bytes included), `EventMeta` (meta only), a `Payload` view `P` (payload only), `Event<P>` (meta and payload), or a consumer type implementing `FromEnvelope` itself",
    note = "implement `Handler<{I}>` with `async fn handle(&self, input: {I}) -> Result<(), Self::Error>`"
)]
pub trait Handler<I> {
    /// The error this handler reports for a failed delivery.
    type Error;

    /// Handles one delivery, received as `I`.
    fn handle(&self, input: I) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;
}

// The closure blanket returns the closure's future directly. Wrapping it in
// an `async fn` would capture `&self` and the argument across the await and
// demand `F: Sync` and a `Send` argument of the closure for no benefit.
//
// `do_not_recommend` keeps rustc from explaining a missing impl as "the trait
// `Fn(I)` is not implemented": the trait's own `on_unimplemented` message
// names the input and the shape instead.
#[diagnostic::do_not_recommend]
impl<I, F, Fut, E> Handler<I> for F
where
    F: Fn(I) -> Fut,
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
impl<I, H: Handler<I>> Handler<I> for Arc<H> {
    type Error = H::Error;

    fn handle(&self, input: I) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        H::handle(self, input)
    }
}
