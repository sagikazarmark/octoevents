//! Platform-conditional `Send`/`Sync` bounds, and the erased error.
//!
//! On native targets these traits retain ordinary thread-safety guarantees.
//! On `wasm32`, where Cloudflare Workers run on one JavaScript event loop,
//! they impose no bound and allow futures and handlers containing JS values.
//!
//! Every `target_arch = "wasm32"` split in the crate's bounds lives here.
//! Test modules that need tokio are gated on native separately.

use std::{error::Error, future::Future, pin::Pin};

/// The erased error: what every handler's error converts into at
/// registration, and what a [`DispatchError`](crate::DispatchError) holds as
/// its source.
///
/// `Box<dyn Error + Send + Sync>` on native targets, the shape tower, hyper
/// and axum name `BoxError`; `Box<dyn Error>` on `wasm32`, where a Worker's
/// error holds a `JsValue` and is neither `Send` nor `Sync`. A handler
/// returning `Result<(), BoxError>` needs no error enum: `?` converts any
/// `Error + Send + Sync + 'static` into it through std's blanket `From`, and
/// a `String`, a `&str` and an `anyhow::Error` through conversions of their
/// own. A handler with an error type of its own keeps it, and the dispatcher
/// and the receiver ask `Into<BoxError>` of it where it is registered, which
/// every `Error + Send + Sync + 'static` type is, and on `wasm32` every
/// `Error + 'static`; an error holding an `Rc` converts there and is refused
/// natively, where the box it would go into is `Send`.
///
/// One alias rather than the spelled-out box so a consumer's `Result<(),
/// octoevents::BoxError>` compiles for a Worker and for a native server
/// alike, as [`MaybeSend`] does for a handler's future.
#[cfg(not(target_arch = "wasm32"))]
pub type BoxError = Box<dyn Error + Send + Sync>;

/// The erased error: what every handler's error converts into at
/// registration, and what a [`DispatchError`](crate::DispatchError) holds as
/// its source. `Box<dyn Error>` on `wasm32`.
#[cfg(target_arch = "wasm32")]
pub type BoxError = Box<dyn Error>;

/// A boxed future that is `Send` on native targets and unconstrained on
/// `wasm32`.
///
/// The boxed counterpart of [`MaybeSend`]: wherever a handler's future is
/// erased behind a `dyn Future`, this alias carries the same platform split so
/// a `MaybeSend` future can be boxed on either target. The lifetime is what
/// the future borrows: `'static` for the receiver's `tower` future, which owns
/// its state, and the handler's borrow for a dispatcher route, whose future
/// runs while the route table is held.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A boxed future that is `Send` on native targets and unconstrained on
/// `wasm32`.
#[cfg(target_arch = "wasm32")]
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// `Send` on native targets; no requirement on `wasm32`.
///
/// The handler trait declares its future `impl Future + MaybeSend`, so a
/// handler whose future holds single-threaded state across an await compiles
/// for a Worker and is rejected natively at its own `impl`, where the
/// diagnostic names the held type. The handler itself holds nothing, so that
/// this bound and not [`MaybeSync`] is the one refusing it:
///
/// ```compile_fail
/// use std::rc::Rc;
/// use octoevents::{Envelope, Handler};
///
/// struct Counter;
///
/// impl Handler<Envelope> for Counter {
///     type Error = ();
///     async fn handle(&self, _envelope: Envelope) -> Result<(), ()> {
///         let calls = Rc::new(1);
///         std::future::ready(()).await; // `calls` is held across the await
///         drop(calls);
///         Ok(())
///     }
/// }
/// ```
// No error code on the block: rustc reports a future that is not `Send`
// as "future cannot be sent between threads safely", a rendering of E0277
// it emits without the code, so there is none to state.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> MaybeSend for T {}

/// `Send` on native targets; no requirement on `wasm32`.
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}

#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

/// `Sync` on native targets; no requirement on `wasm32`.
///
/// The supertrait of [`Handler`](crate::Handler), so `H: Handler<I>` alone
/// lets an adapter hold `&H` across an await; what that means on each
/// platform is under [`MaybeSync` and
/// `MaybeSend`](crate::Handler#maybesync-and-maybesend). A handler holding
/// `!Sync` state is rejected natively at its own `impl`, where the diagnostic
/// names the field's type. The future here is built by hand rather than with
/// `async fn`, which would capture `&self` and fail [`MaybeSend`] as well, so
/// that the supertrait is the one bound refusing it:
///
/// ```compile_fail,E0277
/// use std::cell::Cell;
/// use std::future::Future;
/// use octoevents::{Envelope, Handler, MaybeSend};
///
/// struct Counter { calls: Cell<u32> }
///
/// impl Handler<Envelope> for Counter {
///     type Error = ();
///     fn handle(&self, _envelope: Envelope) -> impl Future<Output = Result<(), ()>> + MaybeSend {
///         self.calls.set(self.calls.get() + 1);
///         async { Ok(()) }
///     }
/// }
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync + ?Sized> MaybeSync for T {}

/// `Sync` on native targets; no requirement on `wasm32`.
#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}

#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSync for T {}
