//! The `octoevents.dispatch` span records one `outcome` value per dispatch,
//! derived from the [`Outcome`] the dispatcher returns.
//!
//! The four values are a contract dashboards filter on: `ok` and
//! `handler_error` for a matched delivery, `fallback_ok` and `fallback_error`
//! for an unmatched one, whichever tier failed it. The same span wraps
//! `WebhookHandler::handle`, so the receiver's path records the value too.

#![cfg(all(feature = "tracing", not(target_arch = "wasm32")))]

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use octoevents::{
    Action, DecodeError, Dispatcher, Envelope, EventKind, EventMeta, Match, Outcome,
    WebhookHandler as _,
};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Debug, PartialEq)]
enum AppError {
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

/// A view any `pull_request` payload satisfies.
#[derive(serde::Deserialize)]
struct AnyPullRequest {}
octoevents::impl_payload!(AnyPullRequest => EventKind::PullRequest);

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for Capture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn envelope(kind: EventKind, action: Option<Action>) -> Envelope {
    let mut meta = EventMeta::new("delivery", kind);
    meta.action = action;
    Envelope {
        meta,
        raw: Bytes::from_static(b"{}"),
    }
}

/// Runs `dispatch` under a fresh subscriber and returns what the
/// `octoevents.dispatch` span recorded as `outcome` alongside the outcome
/// the call returned.
fn traced<F, T>(dispatch: F) -> (String, T)
where
    F: Future<Output = T>,
{
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_max_level(tracing::Level::TRACE)
        .finish();

    // A current-thread runtime keeps the whole call on the thread that holds
    // the subscriber default, which `with_default` does not carry across awaits.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let returned = tracing::subscriber::with_default(subscriber, || runtime.block_on(dispatch));

    let logged = capture.contents();
    let close = logged
        .lines()
        .find(|line| line.contains("octoevents.dispatch") && line.contains("close"))
        .unwrap_or_else(|| panic!("no dispatch span closed: {logged}"));
    let (_, rest) = close
        .split_once("outcome=\"")
        .unwrap_or_else(|| panic!("no outcome recorded: {close}"));
    let (value, _) = rest.split_once('"').unwrap();
    (value.to_owned(), returned)
}

fn dispatcher() -> Dispatcher<AppError> {
    Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], |_: EventMeta, _: AnyPullRequest| async {
            Ok::<_, AppError>(())
        })
        .on_payload_action([Action::Closed], |_: EventMeta, _: AnyPullRequest| async {
            Err::<(), _>("routed")
        })
        .fallback(|meta: EventMeta| async move {
            if meta.kind == EventKind::Installation {
                Err("unmatched")
            } else {
                Ok(())
            }
        })
        .build()
}

#[test]
fn the_span_records_one_of_four_outcomes_derived_from_the_returned_outcome() {
    let dispatcher = dispatcher();

    let (label, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Opened))));
    assert_eq!(label, "ok");
    assert_eq!(
        outcome,
        Outcome {
            matched: Match::Matched,
            result: Ok(())
        }
    );

    let (label, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Closed))));
    assert_eq!(label, "handler_error");
    assert_eq!(
        outcome,
        Outcome {
            matched: Match::Matched,
            result: Err(AppError::Handler("routed"))
        }
    );

    // Unmatched with the kind known and unknown both read as fallback: the
    // label says whether the delivery was matched, not how the miss came about.
    let (label, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Reopened))));
    assert_eq!(label, "fallback_ok");
    assert_eq!(outcome.matched, Match::UnmatchedAction);

    let (label, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(label, "fallback_ok");
    assert_eq!(outcome.matched, Match::UnmatchedKind);

    let (label, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::Installation, Some(Action::Created))));
    assert_eq!(label, "fallback_error");
    assert_eq!(
        outcome,
        Outcome {
            matched: Match::UnmatchedKind,
            result: Err(AppError::Handler("unmatched"))
        }
    );
}

#[test]
fn a_failure_before_routing_is_labelled_by_the_match_the_route_table_decided() {
    let dispatcher = Dispatcher::<AppError>::builder()
        .always(|_: EventMeta| async { Err::<(), _>("audit") })
        .on_payload(|_: EventMeta, _: AnyPullRequest| async { Ok::<_, AppError>(()) })
        .build();

    // The always tier fails both deliveries before any route or fallback
    // runs. The label follows the match, not the tier that failed: the
    // unmatched one reads as `fallback_error` although no fallback ran, as
    // `fallback_ok` already reads that way for an empty fallback chain.
    let (label, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Opened))));
    assert_eq!(label, "handler_error");
    assert_eq!(
        outcome,
        Outcome {
            matched: Match::Matched,
            result: Err(AppError::Handler("audit"))
        }
    );

    let (label, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(label, "fallback_error");
    assert_eq!(
        outcome,
        Outcome {
            matched: Match::UnmatchedKind,
            result: Err(AppError::Handler("audit"))
        }
    );
}

#[test]
fn the_handle_path_records_the_same_outcome() {
    let dispatcher = dispatcher();

    let (label, result) =
        traced(dispatcher.handle(envelope(EventKind::PullRequest, Some(Action::Closed))));
    assert_eq!(label, "handler_error");
    assert_eq!(result, Err(AppError::Handler("routed")));

    let (label, result) =
        traced(dispatcher.handle(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(label, "fallback_ok");
    assert_eq!(result, Ok(()));
}
