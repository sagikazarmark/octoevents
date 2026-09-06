//! The names of the request headers this crate reads.
//!
//! A transport that receives its headers as a map rather than an
//! `http::HeaderMap` builds a [`HeaderView`] with [`HeaderView::from_lookup`],
//! which asks the map for each of these names in turn. Each constant is also
//! the name of the matching `HeaderView` setter, for a transport that hands
//! the values over one at a time.
//!
//! The names are lowercase, as `http::HeaderMap` stores them. Whether the
//! keys of your map match them is your concern; [`HeaderView`] says what to
//! do when they may not.
//!
//! ```
//! use std::collections::HashMap;
//!
//! use octoevents::HeaderView;
//!
//! // The shape a serverless runtime hands over: a map with lowercase keys.
//! let received: HashMap<String, String> = [
//!     ("x-hub-signature-256", "sha256=..."),
//!     ("x-github-delivery", "72d3162e-cc78-11e3-81ab-4c9367dc0958"),
//!     ("x-github-event", "pull_request"),
//!     ("content-type", "application/json"),
//! ]
//! .into_iter()
//! .map(|(name, value)| (name.to_owned(), value.to_owned()))
//! .collect();
//!
//! // Asks the map for `header::SIGNATURE`, `header::DELIVERY_ID`, and the rest.
//! let headers = HeaderView::from_lookup(|name| received.get(name).map(String::as_str));
//! # let _ = headers;
//! ```
//!
//! [`HeaderView`]: crate::HeaderView
//! [`HeaderView::from_lookup`]: crate::HeaderView::from_lookup

/// `X-Hub-Signature-256`: the HMAC-SHA256 of the body under the webhook
/// secret, which [`Verifier`](crate::Verifier) checks.
///
/// GitHub sends the SHA-1 `X-Hub-Signature` beside it; this crate never
/// reads that one, so it has no constant here.
pub const SIGNATURE: &str = "x-hub-signature-256";

/// `X-GitHub-Delivery`: the GUID of one delivery attempt, which becomes
/// [`EventMeta::delivery_id`](crate::EventMeta::delivery_id).
pub const DELIVERY_ID: &str = "x-github-delivery";

/// `X-GitHub-Event`: the event name, which parses into
/// [`EventMeta::kind`](crate::EventMeta::kind).
pub const EVENT_NAME: &str = "x-github-event";

/// `Content-Type`: must be `application/json` for the body to be accepted.
pub const CONTENT_TYPE: &str = "content-type";

/// `X-GitHub-Hook-Installation-Target-Type`: the resource the webhook is
/// installed on, which parses into
/// [`EventMeta::target_type`](crate::EventMeta::target_type).
pub const TARGET_TYPE: &str = "x-github-hook-installation-target-type";

/// `X-GitHub-Hook-Installation-Target-ID`: the ID of that resource, which
/// parses into [`EventMeta::target_id`](crate::EventMeta::target_id).
pub const TARGET_ID: &str = "x-github-hook-installation-target-id";

#[cfg(test)]
mod tests {
    #[test]
    fn header_names_are_lowercase() {
        // The module docs tell a transport with a string map to lowercase its
        // keys before looking these up; that only works if these are.
        for name in [
            super::SIGNATURE,
            super::DELIVERY_ID,
            super::EVENT_NAME,
            super::CONTENT_TYPE,
            super::TARGET_TYPE,
            super::TARGET_ID,
        ] {
            assert_eq!(name, name.to_ascii_lowercase(), "{name} is not lowercase");
        }
    }
}
