//! Fixtures shared by the crate's unit tests: synthetic envelopes over the
//! fixture corpus and one application error every handler error converts into.
//!
//! Nothing here carries an authentication claim; the receiver's tests sign
//! real requests instead.

#![allow(
    dead_code,
    reason = "each test module uses the subset it needs, and which modules \
              exist depends on the enabled features"
)]

use bytes::Bytes;

use crate::{Action, DecodeError, Envelope, EventKind, EventMeta};

/// A synthetic envelope of `kind` over `raw`, with `"delivery"` as its
/// delivery ID and no action.
pub(crate) fn envelope(kind: EventKind, raw: &'static [u8]) -> Envelope {
    Envelope {
        meta: EventMeta::new("delivery", kind),
        raw: Bytes::from_static(raw),
    }
}

/// [`envelope`] with the action set, as the payload probe would.
pub(crate) fn envelope_with_action(
    kind: EventKind,
    action: Action,
    raw: &'static [u8],
) -> Envelope {
    let mut envelope = envelope(kind, raw);
    envelope.meta.action = Some(action);
    envelope
}

pub(crate) fn pull_request_opened() -> Envelope {
    envelope_with_action(
        EventKind::PullRequest,
        Action::Opened,
        include_bytes!("../tests/fixtures/pull_request.opened.json"),
    )
}

pub(crate) fn check_run_completed() -> Envelope {
    envelope_with_action(
        EventKind::CheckRun,
        Action::Completed,
        include_bytes!("../tests/fixtures/check_run.completed.json"),
    )
}

pub(crate) fn installation_created() -> Envelope {
    envelope_with_action(
        EventKind::Installation,
        Action::Created,
        include_bytes!("../tests/fixtures/installation.created.json"),
    )
}

pub(crate) fn ping() -> Envelope {
    envelope(
        EventKind::Ping,
        include_bytes!("../tests/fixtures/ping.json"),
    )
}

pub(crate) fn unknown() -> Envelope {
    envelope(
        EventKind::Unknown("future_event".into()),
        include_bytes!("../tests/fixtures/unknown.json"),
    )
}

/// A `pull_request` delivery octocrab cannot represent, while a consumer view
/// over the same bytes still decodes.
pub(crate) fn unrepresentable() -> Envelope {
    envelope(
        EventKind::PullRequest,
        include_bytes!("../tests/fixtures/unrepresentable.json"),
    )
}

/// The application error a dispatcher under test converts every handler's
/// error into.
#[derive(Debug, PartialEq)]
pub(crate) enum AppError {
    Decode,
    Handler(&'static str),
}

impl From<DecodeError> for AppError {
    fn from(_: DecodeError) -> Self {
        Self::Decode
    }
}

impl From<&'static str> for AppError {
    fn from(message: &'static str) -> Self {
        Self::Handler(message)
    }
}

impl From<std::convert::Infallible> for AppError {
    fn from(never: std::convert::Infallible) -> Self {
        match never {}
    }
}
