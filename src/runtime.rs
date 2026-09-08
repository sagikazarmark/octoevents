//! Platform-conditional `Send`/`Sync` bounds.
//!
//! On native targets these traits retain ordinary thread-safety guarantees.
//! On `wasm32`, where Cloudflare Workers run on one JavaScript event loop,
//! they impose no bound and allow futures and handlers containing JS values.
//!
//! Every `target_arch = "wasm32"` split in the crate's bounds lives here,
//! except the `dyn Fn` alias for the receiver's error observer in `service`,
//! which cannot be expressed through these traits because a trait object
//! admits only one non-auto trait. Test modules that need tokio are gated on
//! native separately.

use std::{future::Future, pin::Pin};

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
/// handler holding single-threaded state compiles for a Worker and is
/// rejected natively at its own `impl`, where the diagnostic names the
/// field's type:
///
/// ```compile_fail
/// use std::{cell::Cell, rc::Rc};
/// use octoevents::{Envelope, Handler};
///
/// struct Counter { calls: Rc<Cell<u32>> }
///
/// impl Handler<Envelope> for Counter {
///     type Error = ();
///     async fn handle(&self, _envelope: Envelope) -> Result<(), ()> {
///         self.calls.set(self.calls.get() + 1);
///         Ok(())
///     }
/// }
/// ```
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
