use serde::de::DeserializeOwned;

use crate::{DecodeError, Envelope, EventKind};

/// An event handler's input, decoded from an [`Envelope`].
///
/// This is the bound on what an [`EventHandler`](crate::EventHandler)
/// receives beside the [`EventMeta`](crate::EventMeta), and what
/// `Dispatcher::on` accepts a handler over. The decode sees the whole
/// envelope, kind included, so an input can check the kind, read the
/// payload, or ignore both. Three impls ship:
///
/// - Every [`Payload`] decodes with [`Envelope::decode_payload`]: the kind
///   check first, then the bytes. A payload registered with `on` under a
///   matcher that disagrees with its kind fails the delivery at the kind, as
///   [`DecodeError::KindMismatch`], not at a missing field.
/// - `()` decodes nothing and cannot fail, so a handler over it is routed by
///   kind and action and receives only the event meta: `installation.deleted`
///   revoking tokens by installation ID needs no payload at all.
/// - octocrab's `WebhookEvent`, with the `octocrab` feature, decodes the
///   payload of any kind into octocrab's model through
///   `Envelope::decode_event`.
///
/// The trait is open. A view over fields several kinds share implements it
/// directly, decoding with [`Envelope::decode`], and is then registered under
/// those kinds with `on`; no octocrab is needed for cross-kind logic:
///
/// ```
/// use octoevents::{DecodeError, Dispatcher, Envelope, EventKind, EventMeta, FromEnvelope};
///
/// /// The sender's login, which every kind carries.
/// #[derive(serde::Deserialize)]
/// struct Sender { sender: Login }
/// #[derive(serde::Deserialize)]
/// struct Login { login: String }
///
/// impl FromEnvelope for Sender {
///     fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
///         envelope.decode()
///     }
/// }
///
/// # #[derive(Debug)]
/// # struct AppError;
/// # impl From<DecodeError> for AppError { fn from(_: DecodeError) -> Self { Self } }
/// # impl From<std::convert::Infallible> for AppError {
/// #     fn from(never: std::convert::Infallible) -> Self { match never {} }
/// # }
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .on([EventKind::Issues, EventKind::IssueComment], |meta: EventMeta, sender: Sender| async move {
///         println!("{} {} by {}", meta.delivery_id, meta.kind, sender.sender.login);
///         Ok::<_, std::convert::Infallible>(())
///     })
///     .build();
/// # let _ = dispatcher;
/// ```
///
/// A serde type that implements neither `Payload` nor `FromEnvelope` is
/// reported with both routes to becoming one:
///
/// ```compile_fail,E0277
/// use octoevents::FromEnvelope;
///
/// fn assert_input<P: FromEnvelope>() {}
///
/// #[derive(serde::Deserialize)]
/// struct Sender { sender: String }
/// assert_input::<Sender>();
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be decoded from an `Envelope`",
    label = "expected a `Payload`, `()`, or a type that implements `FromEnvelope` itself",
    note = "for a serde view over one kind, declare the kind with `octoevents::impl_payload!({Self} => EventKind::..)`: every `Payload` is a `FromEnvelope`",
    note = "for a view over several kinds, implement `FromEnvelope` for `{Self}` directly, decoding with `Envelope::decode`"
)]
pub trait FromEnvelope: Sized {
    /// Decodes the handler's input from the envelope.
    ///
    /// # Errors
    ///
    /// Returns the [`DecodeError`] the dispatcher reports at the handler
    /// that needed this input, converted into the application error through
    /// `From`.
    fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError>;
}

// `do_not_recommend` keeps rustc from explaining a type that is neither a
// payload nor a `FromEnvelope` as "not a payload" through this impl: the
// trait's own message names both routes to becoming one.
#[diagnostic::do_not_recommend]
impl<T: Payload> FromEnvelope for T {
    fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
        envelope.decode_payload()
    }
}

impl FromEnvelope for () {
    fn from_envelope(_: &Envelope) -> Result<Self, DecodeError> {
        Ok(())
    }
}

/// One event kind's decoded payload.
///
/// A `Payload` type declares the kind it belongs to, so a
/// [`EventHandler`](crate::EventHandler) over it is bound to that kind by
/// its type: registering it with `on_payload` needs no matcher, and it cannot
/// be registered under the wrong kind. The whole JSON document GitHub sends
/// is decoded into the type, so a payload type is free to name only the
/// fields it needs. Every payload is a [`FromEnvelope`] whose decode checks
/// the kind first.
///
/// Implement it for your own serde view with [`impl_payload!`]; with the
/// `octocrab` feature, octocrab's per-kind payload structs implement it
/// already.
///
/// Views are the design, rather than one struct per kind owned by this crate.
/// GitHub's payloads differ by action and gain fields over time, so a
/// library's hand-written struct per kind is perpetually behind; a view names
/// the fields its handler reads and ignores the rest, so a field GitHub adds
/// or drops elsewhere in the document changes nothing. A handler that wants
/// the kind's full model uses octocrab's struct for the kind, which is a
/// payload like any other, though those structs mostly leave the top-level
/// `installation`, `sender`, `repository` and `organization` objects to
/// octocrab's `WebhookEvent`; each impl's docs say where to find them.
///
/// ```
/// use octoevents::{EventKind, Payload};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber {
///     number: u64,
/// }
///
/// octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
///
/// assert_eq!(PullRequestNumber::KIND, EventKind::PullRequest);
/// ```
///
/// A serde type that has not declared its kind is reported as not a payload,
/// with the macro call that makes it one:
///
/// ```compile_fail,E0277
/// use octoevents::Payload;
///
/// fn assert_payload<P: Payload>() {}
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber { number: u64 }
/// assert_payload::<PullRequestNumber>();
/// ```
///
/// [`impl_payload!`]: crate::impl_payload
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a payload",
    label = "expected a `serde::Deserialize` type that declares the event kind it decodes",
    note = "declare the kind with `octoevents::impl_payload!({Self} => EventKind::..)`",
    note = "with the `octocrab` feature, octocrab's per-kind `*WebhookEventPayload` structs are payloads; its `WebhookEvent` decodes for any kind, so it is a `FromEnvelope` registered with `on` instead"
)]
pub trait Payload: DeserializeOwned {
    /// The event kind whose deliveries decode into this type.
    const KIND: EventKind;
}

/// Declares which [`EventKind`] each listed type is the payload of.
///
/// Each entry is `Type => kind_expression`, producing an `impl Payload` for
/// the type. Several entries may be listed, separated by commas.
///
/// ```
/// use octoevents::EventKind;
///
/// #[derive(serde::Deserialize)]
/// struct IssueView { action: String }
///
/// #[derive(serde::Deserialize)]
/// struct CommentView { action: String }
///
/// octoevents::impl_payload! {
///     IssueView => EventKind::Issues,
///     CommentView => EventKind::IssueComment,
/// }
/// ```
///
/// [`EventKind`]: crate::EventKind
#[macro_export]
macro_rules! impl_payload {
    ($($payload:ty => $kind:expr),+ $(,)?) => {
        $(
            impl $crate::Payload for $payload {
                const KIND: $crate::EventKind = $kind;
            }
        )+
    };
}
