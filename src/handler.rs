use std::{future::Future, marker::PhantomData};

#[cfg(feature = "octocrab")]
use octocrab::models::webhook_events::WebhookEvent;
use thiserror::Error;

use crate::{Envelope, EventKind, EventMeta, MaybeSend, MaybeSync, Payload};

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
pub trait WebhookHandler {
    /// The error this handler reports for a failed delivery.
    type Error;

    /// Handles one verified envelope.
    fn handle(
        &self,
        envelope: Envelope,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;
}

// Each closure blanket (here and on the two typed traits below) returns the
// closure's future directly. Wrapping it in an `async fn` would capture
// `&self` and the arguments across the await and demand `F: Sync` and
// `Send` arguments of the closure for no benefit.
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
/// annotate its parameter types and, where nothing else fixes it, its error
/// type (`Ok::<_, E>(())`).
///
/// Enabling the `octocrab` feature makes octocrab's pre-1.0 version part of
/// this crate's public API: `WebhookEvent` is octocrab's type, so an octocrab
/// major bump here is a breaking change for this trait and for the
/// `Dispatcher` built on it.
///
/// [`WebhookEvent`]: octocrab::models::webhook_events::WebhookEvent
#[cfg(feature = "octocrab")]
#[cfg_attr(docsrs, doc(cfg(feature = "octocrab")))]
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
#[cfg_attr(docsrs, doc(cfg(feature = "octocrab")))]
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
#[cfg_attr(docsrs, doc(cfg(feature = "octocrab")))]
pub struct EventAdapter<H> {
    handler: H,
}

#[cfg(feature = "octocrab")]
#[cfg_attr(docsrs, doc(cfg(feature = "octocrab")))]
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
/// (`Ok::<_, E>(())`).
///
/// Register one on a `Dispatcher` with `handle_with`, or hand it to the
/// receiver directly through [`PayloadHandler::into_webhook_handler`].
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
    /// The adapter rejects an envelope whose kind is not `P::KIND` with
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

impl<P, H> WebhookHandler for PayloadAdapter<P, H>
where
    P: Payload,
    H: PayloadHandler<P> + MaybeSync,
{
    type Error = HandleError<H::Error>;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        if envelope.meta.kind != P::KIND {
            return Err(HandleError::Decode(DecodeError::KindMismatch {
                expected: P::KIND,
                actual: envelope.meta.kind,
            }));
        }
        let payload = envelope
            .decode_payload::<P>()
            .map_err(HandleError::Decode)?;
        self.handler
            .handle(envelope.meta, payload)
            .await
            .map_err(HandleError::Handler)
    }
}

// The decode steps shared by the adapters (receiver path) and the dispatcher's
// routes. Crate-private: decoding is a step inside handling, not a consumer
// operation, and `Envelope::parse` already covers ad-hoc views.
impl Envelope {
    /// Decodes the payload as `P` without checking the kind; callers that
    /// have not already routed by kind check it first.
    pub(crate) fn decode_payload<P: Payload>(&self) -> Result<P, DecodeError> {
        serde_json::from_slice(&self.raw).map_err(DecodeError::Json)
    }

    /// Decodes the payload as octocrab's `WebhookEvent` for the envelope's
    /// kind.
    #[cfg(feature = "octocrab")]
    pub(crate) fn decode_event(&self) -> Result<WebhookEvent, DecodeError> {
        self.parse_typed().map_err(DecodeError::Json)
    }
}

/// Why an envelope could not be turned into a typed handler's input.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The envelope is of a kind the handler's payload type does not cover.
    #[error("expected a {expected} event, received {actual}")]
    KindMismatch {
        /// The kind the payload type declares.
        expected: EventKind,
        /// The kind of the envelope that arrived.
        actual: EventKind,
    },
    /// The payload did not decode into the expected type.
    #[error("payload could not be decoded")]
    Json(#[source] serde_json::Error),
}

/// The error of an adapted typed handler: either the envelope could not be
/// decoded into the handler's input, or the handler itself failed.
///
/// The handler's own error type is carried as is; it is not required to
/// implement any conversion.
#[derive(Debug, Error)]
pub enum HandleError<E> {
    /// The envelope could not be decoded, so the handler did not run.
    #[error("payload could not be decoded for the handler")]
    Decode(#[source] DecodeError),
    /// The handler ran and failed.
    #[error("handler failed")]
    Handler(#[source] E),
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use bytes::Bytes;

    use super::{DecodeError, HandleError, PayloadHandler, WebhookHandler};
    use crate::{Envelope, EventKind, EventMeta};

    #[derive(serde::Deserialize)]
    struct Opened {
        action: String,
    }

    crate::impl_payload!(Opened => EventKind::Issues);

    fn envelope(kind: EventKind, raw: &'static [u8]) -> Envelope {
        Envelope {
            meta: EventMeta::new("delivery", kind),
            raw: Bytes::from_static(raw),
        }
    }

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
}
