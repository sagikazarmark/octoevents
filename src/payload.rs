use serde::de::DeserializeOwned;

use crate::EventKind;

/// One event kind's decoded payload.
///
/// A `Payload` type declares the kind it belongs to, so a
/// [`PayloadHandler`](crate::PayloadHandler) over it is bound to that kind by
/// its type: registering it needs no matcher, and it cannot be registered
/// under the wrong kind. The whole JSON document GitHub sends is decoded
/// into the type, so a payload type is free to name only the fields it needs.
///
/// Implement it for your own serde view with [`impl_payload!`]; with the
/// `octocrab` feature, octocrab's per-kind payload structs implement it
/// already.
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
/// [`impl_payload!`]: crate::impl_payload
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
