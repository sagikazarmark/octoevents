//! Platform-conditional `Send`/`Sync` bounds.
//!
//! On native targets these traits retain ordinary thread-safety guarantees.
//! On `wasm32`, where Cloudflare Workers run on one JavaScript event loop,
//! they impose no bound and allow futures and handlers containing JS values.

/// `Send` on native targets; no requirement on `wasm32`.
///
/// The handler traits declare their futures `impl Future + MaybeSend`, so a
/// handler holding single-threaded state compiles for a Worker and is
/// rejected natively at its own `impl`, where the diagnostic names the field:
///
/// ```compile_fail
/// use std::{cell::Cell, rc::Rc};
/// use octoevents::{Envelope, WebhookHandler};
///
/// struct Counter { calls: Rc<Cell<u32>> }
///
/// impl WebhookHandler for Counter {
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
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync + ?Sized> MaybeSync for T {}

/// `Sync` on native targets; no requirement on `wasm32`.
#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}

#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSync for T {}
