use serde::de::DeserializeOwned;

use crate::{DecodeError, Envelope, EventKind, EventMeta};

/// A handler's input, decoded from an [`Envelope`].
///
/// This is the bound on what a [`Handler`](crate::Handler) receives, and what
/// `Dispatcher::on` accepts a handler over. The decode sees the whole
/// envelope, kind included, so an input can check the kind, read the payload,
/// copy the meta, or take the envelope whole. The shipped impls:
///
/// - [`Envelope`] is its own input: a clone, the meta plus a refcount bump on
///   the bytes. The receiver and the `always` and `fallback` tiers take a
///   handler over it and move the envelope in without going through here.
/// - [`EventMeta`] decodes nothing and cannot fail, so a handler over it is
///   routed by kind and action and receives only the meta:
///   `installation.deleted` revoking tokens by installation ID needs no
///   payload at all.
/// - Every serde [`Payload`] decodes with [`Envelope::decode_payload`]: the
///   kind check first, then the bytes. A payload registered with `on` under a
///   matcher that disagrees with its kind fails the delivery at the kind, as
///   [`DecodeError::KindMismatch`], not at a missing field.
/// - [`Event<P>`] pairs the meta with any other input's decode.
/// - octocrab's `WebhookEvent`, with the `octocrab` feature, decodes the
///   payload of any kind into octocrab's model through
///   `Envelope::decode_event`.
///
/// The trait is open. A view over fields several kinds share implements it
/// directly, decoding with [`Envelope::decode`], which checks nothing about
/// the kind, and is then registered under those kinds with `on`; no octocrab
/// is needed for cross-kind logic:
///
/// ```
/// use octoevents::{DecodeError, Dispatcher, Envelope, EventKind, FromEnvelope};
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
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .on([EventKind::Issues, EventKind::IssueComment], |sender: Sender| async move {
///         println!("by {}", sender.sender.login);
///         Ok::<_, AppError>(())
///     })
///     .build();
/// # let _ = dispatcher;
/// ```
///
/// An input need not decode the payload at all. One read off the meta makes a
/// field the handler requires part of its type, so a delivery without it fails
/// at the decode, at the handler's registration, instead of every handler
/// unwrapping an `Option`. The failure is neither a kind mismatch nor a JSON
/// error, so the input reports its own reason as [`DecodeError::Input`],
/// whose `Display` is the message verbatim:
///
/// ```
/// use octoevents::{DecodeError, Envelope, FromEnvelope};
///
/// /// The installation ID, required rather than optional.
/// struct InstallationId(u64);
///
/// impl FromEnvelope for InstallationId {
///     fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
///         envelope
///             .meta
///             .installation_id
///             .map(Self)
///             .ok_or_else(|| DecodeError::input("payload has no installation"))
///     }
/// }
/// ```
///
/// A serde type that implements neither `Payload` nor `FromEnvelope` is
/// reported with both routes to becoming an input:
///
/// ```compile_fail,E0277
/// use octoevents::FromEnvelope;
///
/// fn assert_input<I: FromEnvelope>() {}
///
/// #[derive(serde::Deserialize)]
/// struct Sender { sender: String }
/// assert_input::<Sender>();
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be decoded from an `Envelope`",
    label = "expected `Envelope`, `EventMeta`, a `Payload`, `Event<P>`, or a type that implements `FromEnvelope` itself",
    note = "for a serde view over one kind, declare the kind on the type with `#[derive(Payload)] #[payload(EventKind::..)]`: every serde `Payload` is a `FromEnvelope`",
    note = "for a view over several kinds, implement `FromEnvelope` for `{Self}` directly, decoding with `Envelope::decode`"
)]
pub trait FromEnvelope: Sized {
    /// Decodes the handler's input from the envelope.
    ///
    /// # Errors
    ///
    /// Returns the [`DecodeError`] the dispatcher reports at the handler
    /// that needed this input, converted into the application error through
    /// `From`: [`Envelope::decode_payload`] produces the kind mismatch,
    /// [`Envelope::decode`] and `decode_payload` the JSON error, and
    /// [`DecodeError::input`] a reason of the input's own.
    fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError>;
}

// Every serde payload decodes through `decode_payload`: the kind check, then
// the bytes. The serde bound is here rather than on `Payload` so that
// `Event<P>`, which is a `Payload` but not a serde type, has its own impl
// below without overlapping this one: `Event<P>: DeserializeOwned` is
// knowably false, since `Event` is local and derives no `Deserialize`.
//
// `do_not_recommend` keeps rustc from explaining a type that is neither a
// payload nor a `FromEnvelope` as "not a payload" through this impl: the
// trait's own message names both routes to becoming one.
#[diagnostic::do_not_recommend]
impl<T: Payload + DeserializeOwned> FromEnvelope for T {
    fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
        envelope.decode_payload()
    }
}

impl FromEnvelope for EventMeta {
    fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
        Ok(envelope.meta.clone())
    }
}

impl FromEnvelope for Envelope {
    fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
        Ok(envelope.clone())
    }
}

/// The envelope decoded for one handler, the [`EventMeta`] beside the payload
/// decoded as `P`.
///
/// Distinct from [`Envelope`], whose payload is bytes: here the payload is
/// already decoded, and the handler has one source of truth for it. `P` is
/// any [`FromEnvelope`]; the usual one is a [`Payload`] view, and
/// `Event<P>` is then a `Payload` of the same kind, so `on` takes a handler
/// over it under actions alone exactly as it takes one over `P`.
///
/// A parameter destructures it in place, giving the two halves names without
/// a second statement; taking it whole and reading `event.meta` and
/// `event.payload` is the same thing:
///
#[cfg_attr(feature = "derive", doc = "```")]
#[cfg_attr(not(feature = "derive"), doc = "```ignore")]
/// use octoevents::{Event, EventKind, Payload};
///
/// #[derive(serde::Deserialize, Payload)]
/// #[payload(EventKind::PullRequest)]
/// struct PullRequestNumber { number: u64 }
///
/// async fn notify(Event { meta, payload }: Event<PullRequestNumber>) -> Result<(), std::io::Error> {
///     println!("{}: PR #{} {:?}", meta.delivery_id, payload.number, meta.action);
///     Ok(())
/// }
///
/// async fn log(event: Event<PullRequestNumber>) -> Result<(), std::io::Error> {
///     println!("{}: PR #{}", event.meta.delivery_id, event.payload.number);
///     Ok(())
/// }
/// # fn assert_handler<I, H: octoevents::Handler<I>>(_: H) {}
/// # assert_handler(notify);
/// # assert_handler(log);
/// ```
///
/// Both fields are public and the struct is not `#[non_exhaustive]`, so a
/// test builds one by hand and a consumer crate destructures it without
/// `..`. There is no `Deref` to `P`: the payload is `payload`, so a view
/// field named `meta` is never shadowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event<P> {
    /// The delivery's routing metadata.
    pub meta: EventMeta,
    /// The envelope decoded as `P`.
    pub payload: P,
}

impl<P: FromEnvelope> FromEnvelope for Event<P> {
    fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
        Ok(Self {
            meta: envelope.meta.clone(),
            payload: P::from_envelope(envelope)?,
        })
    }
}

/// `Event<P>` is routed by the kind `P` declares, so `on` under actions alone
/// takes a handler over either.
impl<P: Payload> Payload for Event<P> {
    const KIND: EventKind = P::KIND;
}

/// One event kind's decoded payload.
///
/// A `Payload` type declares the kind it belongs to, so a
/// [`Handler`](crate::Handler) over it, or over [`Event<P>`] of it, is bound
/// to that kind by its type: `on` takes it under actions alone, an
/// [`Action`](crate::Action), an array of them or [`AnyAction`](crate::AnyAction),
/// with no kind said, and it cannot be registered under the wrong kind. The whole JSON
/// document GitHub sends is decoded into the type, so a payload type is free
/// to name only the fields it needs. Every serde payload is a
/// [`FromEnvelope`] whose decode checks the kind first.
///
/// Derive it on your own serde view, naming the kind in the `#[payload]`
/// attribute beside the fields it describes; with the `octocrab` feature,
/// octocrab's per-kind payload structs implement it already.
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
#[cfg_attr(feature = "derive", doc = "```")]
#[cfg_attr(not(feature = "derive"), doc = "```ignore")]
/// use octoevents::{Event, EventKind, Payload};
///
/// #[derive(serde::Deserialize, Payload)]
/// #[payload(EventKind::PullRequest)]
/// struct PullRequestNumber {
///     number: u64,
/// }
///
/// assert_eq!(PullRequestNumber::KIND, EventKind::PullRequest);
/// assert_eq!(<Event<PullRequestNumber>>::KIND, EventKind::PullRequest);
/// ```
///
/// The derive expands to the impl below, with `Self: DeserializeOwned` as
/// its where clause, and nothing else. The bound is what makes a serde type a
/// `FromEnvelope`, so a generic view `View<T>` is a payload wherever `View<T>`
/// deserializes, with nothing said about `T` beyond what the type declares.
/// It comes with the `derive` feature, on by default; without the feature,
/// the impl is written by hand, and on a type with no generics the bound
/// goes without saying:
///
/// ```
/// use octoevents::{EventKind, Payload};
///
/// #[derive(serde::Deserialize)]
/// struct PullRequestNumber {
///     number: u64,
/// }
///
/// impl Payload for PullRequestNumber {
///     const KIND: EventKind = EventKind::PullRequest;
/// }
/// ```
///
/// A serde type that has not declared its kind is reported as not a payload,
/// with the derive that makes it one:
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
/// The derive without its attribute is refused at the type name, "missing
/// `#[payload(EventKind::..)]`: a payload declares the kind it decodes":
///
#[cfg_attr(feature = "derive", doc = "```compile_fail")]
#[cfg_attr(not(feature = "derive"), doc = "```ignore")]
/// use octoevents::Payload;
///
/// #[derive(serde::Deserialize, Payload)]
/// struct PullRequestNumber { number: u64 }
/// ```
///
/// An input that declares no kind, the [`EventMeta`] or the [`Envelope`], is
/// reported the same way, and the first note says to register a handler over
/// it with `on` and a matcher instead:
///
/// ```compile_fail,E0277
/// use octoevents::{EventMeta, Payload};
///
/// fn assert_payload<P: Payload>() {}
///
/// assert_payload::<EventMeta>();
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a payload",
    label = "expected a `serde::Deserialize` type that declares the event kind it decodes, or `Event<P>` over one",
    note = "`Envelope`, `EventMeta` and a view over several kinds declare no kind: a handler over them is registered with `on` and a matcher that says the kind",
    note = "a serde view over one kind declares it on the type: `#[derive(Payload)] #[payload(EventKind::..)]`",
    note = "with the `octocrab` feature, octocrab's per-kind `*WebhookEventPayload` structs are payloads; its `WebhookEvent` decodes for any kind, so it is a `FromEnvelope` registered with `on` instead"
)]
pub trait Payload: FromEnvelope {
    /// The event kind whose deliveries decode into this type.
    const KIND: EventKind;
}
