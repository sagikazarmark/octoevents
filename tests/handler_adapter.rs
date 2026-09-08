//! Compile-time coverage for an adapter generic over another handler.
//!
//! An adapter wrapping a handler in a struct generic over `H: Handler<..>`
//! holds `&self.inner` across the await in its own `handle`, so its future is
//! `Send` only if `H` is `Sync`. `Handler<I>: MaybeSync` states that once, on
//! the trait, so the adapter compiles as first written, with no bound on `H`
//! beyond the trait, on native targets and on `wasm32`. Run it natively with
//! `cargo test --test handler_adapter` and build it for `wasm32` with
//! `cargo build --test handler_adapter --target wasm32-unknown-unknown`. On
//! `wasm32` the same adapter also wraps a handler holding `Rc` state, which
//! `H: Sync`, the bound rustc would otherwise suggest, refuses.
//!
//! The input is another matter: an `async fn`'s future owns its arguments
//! from creation, so an adapter generic over the payload bounds it
//! `MaybeSend`, the platform-conditional spelling of the `P: Send` rustc
//! suggests. That bound is on the input, not the handler, and no supertrait
//! can imply it.

use std::convert::Infallible;

use octoevents::{
    Action, AnyAction, DecodeError, Dispatcher, Event, EventKind, Handler, MaybeSend,
};

/// Forwards to `inner` once it has read the meta: the shape of an audit,
/// timing or retry adapter. `H` carries no bound beyond the trait.
struct Audited<H> {
    inner: H,
}

impl<P, H> Handler<Event<P>> for Audited<H>
where
    P: MaybeSend,
    H: Handler<Event<P>>,
{
    type Error = H::Error;

    async fn handle(&self, event: Event<P>) -> Result<(), Self::Error> {
        let delivery_id = event.meta.delivery_id.clone();
        let result = self.inner.handle(event).await;
        if result.is_err() {
            println!("{delivery_id} failed");
        }
        result
    }
}

/// Written by hand rather than derived so the file compiles under
/// `--no-default-features` too; the derive has its own test file.
#[derive(serde::Deserialize)]
struct PullRequestNumber {
    number: u64,
}
impl octoevents::Payload for PullRequestNumber {
    const KIND: EventKind = EventKind::PullRequest;
}

/// The application error a dispatcher under test converts every handler's
/// error into.
struct AppError;

impl From<DecodeError> for AppError {
    fn from(_: DecodeError) -> Self {
        Self
    }
}

impl From<Infallible> for AppError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}

#[test]
fn an_adapter_over_a_handler_needs_no_bound_on_it_beyond_the_trait() {
    async fn label(Event { payload, .. }: Event<PullRequestNumber>) -> Result<(), Infallible> {
        println!("label PR #{}", payload.number);
        Ok(())
    }

    let _dispatcher = Dispatcher::<AppError>::builder()
        .on(AnyAction, Audited { inner: label })
        .on([Action::Opened], Audited { inner: label })
        .on(EventKind::PullRequest, Audited { inner: label })
        .build();
}

/// A Worker hands its handlers JavaScript values and `Rc` state, so `H` in the
/// adapter is neither `Send` nor `Sync` there; `MaybeSync` asks nothing of it
/// on `wasm32`, where `H: Sync` would have refused it.
#[cfg(target_arch = "wasm32")]
#[test]
fn the_adapter_wraps_a_single_threaded_handler_on_wasm32() {
    use std::{cell::Cell, rc::Rc};

    struct Counter {
        calls: Rc<Cell<u32>>,
    }

    // Bumps a counter instead of awaiting a JavaScript binding, which is what
    // a real `async fn handle` would do.
    #[expect(clippy::unused_async_trait_impl)]
    impl Handler<Event<PullRequestNumber>> for Counter {
        type Error = Infallible;

        async fn handle(&self, _event: Event<PullRequestNumber>) -> Result<(), Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    let _dispatcher = Dispatcher::<AppError>::builder()
        .on(
            AnyAction,
            Audited {
                inner: Counter {
                    calls: Rc::new(Cell::new(0)),
                },
            },
        )
        .build();
}
