use std::{fmt, future::Future, marker::PhantomData, sync::Arc};

#[cfg(feature = "octocrab")]
use octocrab::models::webhook_events::WebhookEvent;
use thiserror::Error;

use crate::{DecodeError, Envelope, EventMeta, MaybeSend, MaybeSync, Payload};

/// Consumer-owned code that handles one verified [`Envelope`].
///
/// This is the handler flavour the receiver accepts. Implement it on a struct
/// whose fields are its dependencies and write a plain `async fn handle`; the
/// future borrows `&self`, so nothing is cloned per delivery:
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
/// Closures implement the trait too. Annotate the parameter type and, where
/// nothing else fixes it, the error type:
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
///   = note: a meta, event or payload handler reaches the receiver through its `into_webhook_handler()`
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
    note = "a webhook handler receives the verified `Envelope`: implement `WebhookHandler` with `async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error>`",
    note = "a meta, event or payload handler reaches the receiver through its `into_webhook_handler()`"
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

// Each closure blanket (here and on the three other flavours below) returns
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

/// Consumer-owned code that handles one delivery's [`EventMeta`] alone.
///
/// A meta handler receives the routing metadata and nothing else: no payload
/// bytes and no decoded payload. With nothing to decode it runs for every
/// verified delivery, including one whose payload no typed handler can
/// decode, which makes it the flavour for audit, metrics, deduplication, and
/// rejection: logic that reads only the fields [`EventMeta`] already carries.
///
/// ```
/// use octoevents::{EventMeta, MetaHandler};
///
/// struct Dedup { /* seen delivery IDs */ }
///
/// impl MetaHandler for Dedup {
///     type Error = std::io::Error;
///
///     async fn handle(&self, meta: EventMeta) -> Result<(), Self::Error> {
///         println!("{} {} from {:?}", meta.delivery_id, meta.kind, meta.sender);
///         Ok(())
///     }
/// }
/// ```
///
/// A closure `Fn(EventMeta) -> Fut` is a meta handler too; annotate its
/// parameter type and, where nothing else fixes it, its error type
/// (`Ok::<_, E>(())`):
///
/// ```
/// use octoevents::{EventMeta, MetaHandler};
///
/// fn log() -> impl MetaHandler<Error = std::convert::Infallible> {
///     |meta: EventMeta| async move {
///         println!("{} {}", meta.delivery_id, meta.kind);
///         Ok::<_, std::convert::Infallible>(())
///     }
/// }
/// ```
///
/// An `Arc<H>` is a meta handler whenever `H` is, so one struct can be shared
/// (between a receiver and a test, say) without a closure adapter. `&H` and
/// `Box<H>` are not: std implements `Fn` for both, so those impls would
/// overlap the closure blanket. Hand one to the receiver through
/// [`MetaHandler::into_webhook_handler`].
///
/// A value that is not a meta handler is reported as such, with the shape
/// expected:
///
/// ```compile_fail,E0277
/// use octoevents::MetaHandler;
///
/// fn assert_handler<H: MetaHandler>(_: H) {}
///
/// struct Dedup;
/// assert_handler(Dedup);
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a meta handler",
    label = "expected an `impl MetaHandler` or a closure `|meta: EventMeta| async {{ .. }}`",
    note = "a meta handler receives only the `EventMeta`: implement `MetaHandler` with `async fn handle(&self, meta: EventMeta) -> Result<(), Self::Error>`"
)]
pub trait MetaHandler {
    /// The error this handler reports for a failed delivery.
    type Error;

    /// Handles one delivery's metadata.
    fn handle(&self, meta: EventMeta) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;

    /// Adapts this handler for the receiver, which accepts only webhook
    /// handlers.
    ///
    /// The adapter drops the payload bytes and passes the handler's error
    /// through unchanged: with nothing to decode there is no decode failure to
    /// report, so its error type is `Self::Error` rather than
    /// [`HandleError`].
    #[must_use]
    fn into_webhook_handler(self) -> MetaAdapter<Self>
    where
        Self: Sized,
    {
        MetaAdapter { handler: self }
    }
}

#[diagnostic::do_not_recommend]
impl<F, Fut, E> MetaHandler for F
where
    F: Fn(EventMeta) -> Fut,
    Fut: Future<Output = Result<(), E>> + MaybeSend,
{
    type Error = E;

    #[allow(refining_impl_trait)]
    fn handle(&self, meta: EventMeta) -> Fut {
        self(meta)
    }
}

// Coherent with the closure blanket because `Fn` is `#[fundamental]`: `Arc<H>`
// does not implement it, and the compiler may assume it never will. Returns
// the inner future directly, so `H` needs no `Send + Sync` bound beyond what
// its own `handle` already states.
impl<H> MetaHandler for Arc<H>
where
    H: MetaHandler + ?Sized,
{
    type Error = H::Error;

    fn handle(&self, meta: EventMeta) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        H::handle(self, meta)
    }
}

/// A [`MetaHandler`] adapted to the [`WebhookHandler`] the receiver accepts;
/// built by [`MetaHandler::into_webhook_handler`].
pub struct MetaAdapter<H> {
    handler: H,
}

// Every adapter's `Debug` elides the handler rather than bounding on it, as
// `WebhookReceiver` does: closures are never `Debug`, and the flavour is what
// is worth printing.
impl<H> fmt::Debug for MetaAdapter<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MetaAdapter")
            .finish_non_exhaustive()
    }
}

impl<H> WebhookHandler for MetaAdapter<H>
where
    H: MetaHandler,
{
    type Error = H::Error;

    // Returns the handler's future directly: nothing is awaited here, so
    // `&self` is not held across an await and `H` needs no `MaybeSync`.
    fn handle(
        &self,
        envelope: Envelope,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        self.handler.handle(envelope.meta)
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
/// annotate its parameter types and, where nothing else fixes it, its error
/// type (`Ok::<_, E>(())`):
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

    /// Adapts this handler for the receiver, which accepts only webhook
    /// handlers.
    ///
    /// The adapter decodes the envelope with octocrab and reports a payload
    /// octocrab cannot represent as [`HandleError::Decode`]. A single-purpose
    /// receiver therefore needs no dispatcher.
    #[must_use]
    fn into_webhook_handler(self) -> EventAdapter<Self>
    where
        Self: Sized,
    {
        EventAdapter { handler: self }
    }
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

/// An [`EventHandler`] adapted to the [`WebhookHandler`] the receiver
/// accepts; built by [`EventHandler::into_webhook_handler`].
#[cfg(feature = "octocrab")]
pub struct EventAdapter<H> {
    handler: H,
}

#[cfg(feature = "octocrab")]
impl<H> fmt::Debug for EventAdapter<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventAdapter")
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "octocrab")]
impl<H> WebhookHandler for EventAdapter<H>
where
    H: EventHandler + MaybeSync,
{
    type Error = HandleError<H::Error>;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        let event = envelope.decode_event().map_err(HandleError::Decode)?;
        self.handler
            .handle(envelope.meta, event)
            .await
            .map_err(HandleError::Handler)
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
/// A closure `Fn(EventMeta, P) -> Fut` is a payload handler too; annotate
/// its parameter types and, where nothing else fixes it, its error type
/// (`Ok::<_, E>(())`):
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
/// Register one on a `Dispatcher` with `handle_with`, or hand it to the
/// receiver directly through [`PayloadHandler::into_webhook_handler`].
///
/// The trait is generic over the payload, so one struct can implement it for
/// several payload types. Registration then needs a turbofish, because the
/// struct alone no longer says which payload is meant:
/// `dispatcher.handle_with::<PullRequestNumber, _>(labeler)` and
/// `PayloadHandler::<PullRequestNumber>::into_webhook_handler(labeler)`. A
/// closure fixes the payload by its parameter type and needs neither.
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

    /// Adapts this handler for the receiver, which accepts only webhook
    /// handlers.
    ///
    /// The adapter decodes with [`Envelope::decode_payload`], so it rejects an
    /// envelope whose kind is not `P::KIND` with
    /// [`DecodeError::KindMismatch`] and reports a payload that does not
    /// decode as [`DecodeError::Json`]; both surface as
    /// [`HandleError::Decode`]. A single-purpose receiver therefore needs no
    /// dispatcher, and a misconfigured webhook fails loudly rather than
    /// passing through.
    #[must_use]
    fn into_webhook_handler(self) -> PayloadAdapter<P, Self>
    where
        Self: Sized,
    {
        PayloadAdapter {
            handler: self,
            payload: PhantomData,
        }
    }
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

/// A [`PayloadHandler`] adapted to the [`WebhookHandler`] the receiver
/// accepts; built by [`PayloadHandler::into_webhook_handler`].
pub struct PayloadAdapter<P, H> {
    handler: H,
    payload: PhantomData<fn() -> P>,
}

impl<P, H> fmt::Debug for PayloadAdapter<P, H>
where
    P: Payload,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The kind is the one thing that tells two payload adapters apart.
        formatter
            .debug_struct("PayloadAdapter")
            .field("kind", &P::KIND)
            .finish_non_exhaustive()
    }
}

impl<P, H> WebhookHandler for PayloadAdapter<P, H>
where
    P: Payload,
    H: PayloadHandler<P> + MaybeSync,
{
    type Error = HandleError<H::Error>;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        let payload = envelope
            .decode_payload::<P>()
            .map_err(HandleError::Decode)?;
        self.handler
            .handle(envelope.meta, payload)
            .await
            .map_err(HandleError::Handler)
    }
}

/// The error of an adapted typed handler: either the envelope could not be
/// decoded into the handler's input, or the handler itself failed.
///
/// The handler's own error type is carried as is; it is not required to
/// implement any conversion. The enum is exhaustive: an adapter can fail in
/// exactly these two ways, so generic code matches both arms and needs no
/// wildcard.
///
/// ```
/// use octoevents::HandleError;
///
/// fn phase<E>(error: &HandleError<E>) -> &'static str {
///     match error {
///         HandleError::Decode(_) => "decode",
///         HandleError::Handler(_) => "handler",
///     }
/// }
/// ```
///
/// When the handler's error type absorbs a [`DecodeError`],
/// [`HandleError::into_error`] collapses both cases into it.
#[derive(Debug, Error)]
pub enum HandleError<E> {
    /// The envelope could not be decoded, so the handler did not run.
    #[error("envelope could not be decoded into the handler's input")]
    Decode(#[source] DecodeError),
    /// The handler ran and failed.
    #[error("handler failed")]
    Handler(#[source] E),
}

impl<E> HandleError<E>
where
    E: From<DecodeError>,
{
    /// Collapses the error into the handler's own error type.
    ///
    /// A decode failure is converted through `From`; a handler failure is
    /// returned as is. This is the one-call path from an adapter's error to
    /// an application error, and it rescues a handler whose error is
    /// `Box<dyn Error + Send + Sync>`: `HandleError` over that type is not
    /// itself an [`Error`](std::error::Error), but the boxed error absorbs a
    /// [`DecodeError`], so the collapsed value is.
    ///
    /// ```
    /// use octoevents::{DecodeError, HandleError};
    ///
    /// #[derive(Debug)]
    /// enum AppError { Decode(DecodeError), Database }
    /// impl From<DecodeError> for AppError {
    ///     fn from(error: DecodeError) -> Self { Self::Decode(error) }
    /// }
    ///
    /// let failed: HandleError<AppError> = HandleError::Handler(AppError::Database);
    /// assert!(matches!(failed.into_error(), AppError::Database));
    /// ```
    pub fn into_error(self) -> E {
        match self {
            Self::Decode(error) => E::from(error),
            Self::Handler(error) => error,
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::{HandleError, MetaHandler, PayloadHandler, WebhookHandler};
    use crate::{
        Action, DecodeError, EventKind, EventMeta,
        test_support::{envelope, unrepresentable},
    };

    #[tokio::test]
    async fn the_meta_adapter_passes_the_metadata_and_the_handler_error_through() {
        #[derive(Debug, PartialEq)]
        struct Private(String);

        // A meta handler has nothing to decode, so its error reaches the
        // caller as is rather than behind `HandleError::Handler`.
        let handler = (|meta: EventMeta| async move { Err::<(), _>(Private(meta.delivery_id)) })
            .into_webhook_handler();

        let error = handler
            .handle(envelope(EventKind::Issues, br#"{"action":"opened"}"#))
            .await
            .unwrap_err();

        assert_eq!(error, Private("delivery".to_owned()));
    }

    #[tokio::test]
    async fn the_meta_adapter_runs_on_a_payload_nothing_can_decode() {
        type Seen = Arc<Mutex<Vec<(String, EventKind, Option<Action>)>>>;

        struct Auditor {
            seen: Seen,
        }

        impl MetaHandler for Auditor {
            type Error = std::convert::Infallible;

            async fn handle(&self, meta: EventMeta) -> Result<(), Self::Error> {
                self.seen
                    .lock()
                    .await
                    .push((meta.delivery_id, meta.kind, meta.action));
                Ok(())
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = Auditor {
            seen: Arc::clone(&seen),
        }
        .into_webhook_handler();

        // octocrab cannot represent this pull request; a meta handler never
        // asks it to.
        handler.handle(unrepresentable()).await.unwrap();

        assert_eq!(
            seen.lock().await.as_slice(),
            [("delivery".to_owned(), EventKind::PullRequest, None)]
        );
    }

    #[tokio::test]
    async fn an_arc_shares_one_meta_handler_between_adapters() {
        struct Counter {
            calls: Arc<Mutex<u32>>,
        }

        impl MetaHandler for Counter {
            type Error = std::convert::Infallible;

            async fn handle(&self, _meta: EventMeta) -> Result<(), Self::Error> {
                *self.calls.lock().await += 1;
                Ok(())
            }
        }

        let calls = Arc::new(Mutex::new(0));
        let shared = Arc::new(Counter {
            calls: Arc::clone(&calls),
        });

        // One struct, two adapters, no closure written by hand.
        let first = Arc::clone(&shared).into_webhook_handler();
        let second = shared.into_webhook_handler();

        first.handle(unrepresentable()).await.unwrap();
        second.handle(unrepresentable()).await.unwrap();

        assert_eq!(*calls.lock().await, 2);
    }

    #[derive(serde::Deserialize)]
    struct Opened {
        action: String,
    }

    crate::impl_payload!(Opened => EventKind::Issues);

    #[tokio::test]
    async fn the_payload_adapter_names_the_kind_mismatch_it_refused() {
        let handler = (|_: EventMeta, _: Opened| async { Ok::<_, ()>(()) }).into_webhook_handler();

        let error = handler
            .handle(envelope(EventKind::PullRequest, br#"{"action":"opened"}"#))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            HandleError::Decode(DecodeError::KindMismatch {
                expected: EventKind::Issues,
                actual: EventKind::PullRequest,
            })
        ));
    }

    #[tokio::test]
    async fn the_payload_adapter_keeps_the_handler_error_as_is() {
        #[derive(Debug, PartialEq)]
        struct Private(String);

        let handler =
            (|_: EventMeta, opened: Opened| async move { Err::<(), _>(Private(opened.action)) })
                .into_webhook_handler();

        let error = handler
            .handle(envelope(EventKind::Issues, br#"{"action":"opened"}"#))
            .await
            .unwrap_err();

        assert!(matches!(error, HandleError::Handler(Private(action)) if action == "opened"));
    }

    #[test]
    fn adapters_are_debug_without_constraining_the_handler() {
        // Closures are never `Debug`, so the adapters elide the handler and
        // print what distinguishes them: the flavour and, for a payload
        // adapter, the kind its payload type declares.
        let meta = (|_: EventMeta| async { Ok::<_, ()>(()) }).into_webhook_handler();
        let payload = (|_: EventMeta, _: Opened| async { Ok::<_, ()>(()) }).into_webhook_handler();

        assert_eq!(format!("{meta:?}"), "MetaAdapter { .. }");
        assert_eq!(
            format!("{payload:?}"),
            "PayloadAdapter { kind: Issues, .. }"
        );
    }

    #[cfg(feature = "octocrab")]
    #[test]
    fn the_event_adapter_is_debug_without_constraining_the_handler() {
        use octocrab::models::webhook_events::WebhookEvent;

        use super::EventHandler as _;

        let event =
            (|_: EventMeta, _: WebhookEvent| async { Ok::<_, ()>(()) }).into_webhook_handler();

        assert_eq!(format!("{event:?}"), "EventAdapter { .. }");
    }

    #[test]
    fn into_error_collapses_both_variants_into_the_handler_error() {
        #[derive(Debug, PartialEq)]
        enum AppError {
            Decode(EventKind),
            Failed(&'static str),
        }

        impl From<DecodeError> for AppError {
            fn from(error: DecodeError) -> Self {
                match error {
                    DecodeError::KindMismatch { actual, .. } => Self::Decode(actual),
                    other => panic!("unexpected {other:?}"),
                }
            }
        }

        let decode: HandleError<AppError> = HandleError::Decode(DecodeError::KindMismatch {
            expected: EventKind::Issues,
            actual: EventKind::PullRequest,
        });
        let failed: HandleError<AppError> = HandleError::Handler(AppError::Failed("boom"));

        assert_eq!(
            decode.into_error(),
            AppError::Decode(EventKind::PullRequest)
        );
        assert_eq!(failed.into_error(), AppError::Failed("boom"));
    }

    #[test]
    fn into_error_rescues_a_boxed_dyn_error_handler() {
        // `HandleError<Box<dyn Error + Send + Sync>>` is not itself an
        // `Error` (thiserror's `#[source]` needs `E: Error + 'static`), but
        // the boxed error absorbs a `DecodeError`, so the collapsed value is.
        type Boxed = Box<dyn std::error::Error + Send + Sync>;

        let decode: HandleError<Boxed> = HandleError::Decode(DecodeError::KindMismatch {
            expected: EventKind::Issues,
            actual: EventKind::PullRequest,
        });
        let failed: HandleError<Boxed> = HandleError::Handler("boom".into());

        assert!(decode.into_error().downcast_ref::<DecodeError>().is_some());
        assert_eq!(failed.into_error().to_string(), "boom");
    }
}
