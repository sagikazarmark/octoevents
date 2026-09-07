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

use crate::{Action, DecodeError, Envelope, EventKind};

/// A synthetic envelope of `kind` over `raw`, with `"delivery"` as its
/// delivery ID and the meta the receiver would have extracted from `raw`.
pub(crate) fn envelope(kind: EventKind, raw: &'static [u8]) -> Envelope {
    Envelope::new("delivery", kind, raw)
}

/// [`envelope`] delivered under `action`, whatever the body says.
///
/// For a body that carries the action this changes nothing. It is the
/// override for a body that does not (a non-JSON body reaching a handler
/// over `EventMeta`) or that carries another one (a fixture delivered under
/// an action the corpus does not cover).
pub(crate) fn envelope_with_action(
    kind: EventKind,
    action: Action,
    raw: &'static [u8],
) -> Envelope {
    let mut envelope = envelope(kind, raw);
    envelope.meta.action = Some(action);
    envelope
}

/// The `pull_request.opened` fixture, its action read from the body.
pub(crate) fn pull_request_opened() -> Envelope {
    envelope(
        EventKind::PullRequest,
        include_bytes!("../tests/fixtures/pull_request.opened.json"),
    )
}

/// The `pull_request.opened` fixture delivered under `action`, so a route
/// table can be tried with actions the corpus does not cover. The body still
/// says `opened`; only the meta is under `action`.
pub(crate) fn pull_request(action: Action) -> Envelope {
    envelope_with_action(
        EventKind::PullRequest,
        action,
        include_bytes!("../tests/fixtures/pull_request.opened.json"),
    )
}

pub(crate) fn check_run_completed() -> Envelope {
    envelope(
        EventKind::CheckRun,
        include_bytes!("../tests/fixtures/check_run.completed.json"),
    )
}

pub(crate) fn installation_created() -> Envelope {
    envelope(
        EventKind::Installation,
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
#[derive(Debug, Clone, PartialEq)]
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
