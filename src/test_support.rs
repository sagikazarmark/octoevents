//! Fixtures shared by the crate's unit tests: synthetic envelopes over the
//! fixture corpus, one application error the tests read a boxed dispatch
//! source back as, and the signed request the authenticating path and the
//! envelope are tested over.
//!
//! The synthetic envelopes carry no authentication claim; the receiver's
//! tests sign real requests instead.

#![allow(
    dead_code,
    reason = "each test module uses the subset it needs, and which modules \
              exist depends on the enabled features"
)]

use std::fmt;

use http::{HeaderMap, HeaderValue};

use crate::{
    Action, BoxError, DecodeError, DispatchError, Envelope, EventKind, HeaderMeta, Verifier,
    WebhookSecret, header,
};

/// A synthetic envelope of `kind` over `payload`, with `"delivery"` as its
/// delivery ID and the meta the receiver would have read from `payload`.
pub(crate) fn envelope(kind: EventKind, payload: impl AsRef<[u8]>) -> Envelope {
    Envelope::new(HeaderMeta::new("delivery", kind), payload)
}

/// [`envelope`] delivered under `action`, whatever the payload says.
///
/// The override for a payload that carries no action (a non-JSON payload
/// reaching a handler over `EventMeta`) or carries another one (a fixture
/// delivered under an action the corpus does not cover). A payload that
/// carries `action` already needs only [`envelope`].
pub(crate) fn envelope_with_action(
    kind: EventKind,
    action: Action,
    payload: &'static [u8],
) -> Envelope {
    let mut envelope = envelope(kind, payload);
    envelope.meta.action = Some(action);
    envelope
}

/// The `pull_request.opened` fixture, its action read from the payload.
pub(crate) fn pull_request_opened() -> Envelope {
    envelope(
        EventKind::PullRequest,
        include_bytes!("../tests/fixtures/pull_request.opened.json"),
    )
}

/// The `pull_request.opened` fixture delivered under `action`, so a route
/// table can be tried with actions the corpus does not cover. The payload
/// still says `opened`; only the meta is under `action`.
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

/// The `installation_repositories.removed` fixture, its action read from the
/// payload.
pub(crate) fn installation_repositories_removed() -> Envelope {
    envelope(
        EventKind::InstallationRepositories,
        include_bytes!("../tests/fixtures/installation_repositories.removed.json"),
    )
}

pub(crate) fn ping() -> Envelope {
    envelope(
        EventKind::Ping,
        include_bytes!("../tests/fixtures/ping.json"),
    )
}

/// The ping fixture's bytes delivered under an event name the crate does not
/// know: a real payload, an unknown kind. No file of its own; the kind is the
/// only difference from [`ping`].
pub(crate) fn unknown() -> Envelope {
    envelope(
        EventKind::from("future_event"),
        include_bytes!("../tests/fixtures/ping.json"),
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

/// The application error a dispatcher under test fails with, and the view a
/// test reads a boxed dispatch source back as.
///
/// A handler under test returns `Handler(name)`, naming itself, so the call
/// log and the error name the same handler. A route whose decode failed
/// carries a boxed [`DecodeError`], which [`from_boxed`](Self::from_boxed)
/// reads back as `Decode`, so a test asserts on one enum whichever failed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AppError {
    Decode,
    Handler(&'static str),
}

impl AppError {
    /// The test's view of a dispatch error's boxed source: the `AppError` the
    /// handler returned, or `Decode` for the `DecodeError` a route's decode
    /// failed with. Anything else fails the test.
    pub(crate) fn from_boxed(error: BoxError) -> Self {
        match error.downcast::<Self>() {
            Ok(error) => *error,
            Err(error) => match error.downcast::<DecodeError>() {
                Ok(_) => Self::Decode,
                Err(other) => panic!("neither an AppError nor a DecodeError: {other}"),
            },
        }
    }
}

/// A dispatch error's boxed source downcast to the type the handler returned;
/// any other type fails the test, naming what it was.
#[track_caller]
pub(crate) fn source_as<E: std::error::Error + 'static>(error: &DispatchError) -> &E {
    error.source.downcast_ref::<E>().unwrap_or_else(|| {
        panic!(
            "the source is not a {}: {}",
            std::any::type_name::<E>(),
            error.source
        )
    })
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode => formatter.write_str("decode"),
            Self::Handler(name) => write!(formatter, "handler {name} failed"),
        }
    }
}

impl std::error::Error for AppError {}

/// The payload of the signed requests the authenticating path and the
/// envelope are tested over: an `opened` delivery carrying every probed field.
pub(crate) const BODY: &[u8] = br#"{
    "action":"opened",
    "installation":{"id":42},
    "repository":{"id":1,"name":"repo","full_name":"octo/repo","owner":{"login":"octo"}},
    "organization":{"id":9919,"login":"github"},
    "sender":{"id":2,"login":"monalisa"}
}"#;

/// The verifier every signed envelope in the tests is checked against; it
/// also signs the bodies it accepts.
pub(crate) fn verifier() -> Verifier {
    Verifier::new(WebhookSecret::new("secret"))
}

/// The headers of a well-formed `pull_request` delivery carrying
/// `signature`, target included; a test that wants one header wrong
/// overrides or removes it.
pub(crate) fn headers(signature: &str) -> HeaderMap {
    HeaderMap::from_iter([
        (header::SIGNATURE, signature.parse().unwrap()),
        (header::DELIVERY_ID, HeaderValue::from_static("delivery")),
        (header::EVENT_NAME, HeaderValue::from_static("pull_request")),
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        ),
        (header::TARGET_TYPE, HeaderValue::from_static("repository")),
        (header::TARGET_ID, HeaderValue::from_static("7")),
    ])
}

/// A header map from `(name, value)` string pairs, each name parsed into a
/// `HeaderName`.
pub(crate) fn headers_from<const N: usize>(entries: [(&str, &str); N]) -> HeaderMap {
    entries
        .into_iter()
        .map(|(name, value)| (name.parse().unwrap(), value.parse().unwrap()))
        .collect()
}
