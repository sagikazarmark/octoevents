//! The README's "Testing without GitHub" section, compiled.
//!
//! The README's Rust blocks are doctests, but the tests in that section
//! continue the complete program above them and are marked `ignore`, so
//! nothing else compiles them. This file is those tests as written, with the
//! program's `AppError`, `IssueOpened` and `label` beside them, and holds the
//! section's prose to what it claims: `Envelope::new` reads the action out of
//! the bytes while `EventMeta::new` on its own reads none, an
//! `http::Request<String>` is a request `receive` accepts with no axum in
//! sight, and a delivery nothing routes reports which unmatched `Match`
//! variant it is. The last test is the hello world's claim: for a
//! `Dispatcher<Box<dyn Error + Send + Sync>>`, a decode failure's serde
//! message is reached from `error.source.source()`.

#![cfg(all(feature = "http", not(target_arch = "wasm32")))]

use std::sync::{Arc, Mutex};

use hmac::{Hmac, KeyInit as _, Mac as _};
use octoevents::{
    Action, DecodeError, DispatchError, Dispatcher, Envelope, Event, EventKind, EventMeta, Match,
    Secret, Verifier, WebhookReceiverBuilder, header,
};
use sha2::Sha256;

/// The complete program's application error.
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

/// The complete program's payload view.
#[derive(serde::Deserialize)]
struct IssueOpened {
    issue: Issue,
}

#[derive(serde::Deserialize)]
struct Issue {
    number: u64,
    title: String,
}

octoevents::impl_payload!(IssueOpened => EventKind::Issues);

/// The complete program's routed handler.
async fn label(issue: IssueOpened) -> Result<(), AppError> {
    println!("label #{} '{}'", issue.issue.number, issue.issue.title);
    Ok(())
}

/// What GitHub puts in `X-Hub-Signature-256`, as the README writes it.
// The README's spelling, kept verbatim; `tests/common::signature` writes the
// hex with `write!` instead, which is what clippy asks for here.
#[allow(clippy::format_collect)]
fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256={hex}")
}

/// The README's first test, verbatim.
#[tokio::test]
async fn labels_an_opened_issue() {
    let dispatcher = Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], label)
        .build();

    let envelope = Envelope::new(
        "delivery-1",
        EventKind::Issues,
        br#"{"action":"opened","issue":{"number":7,"title":"Add tests"}}"#,
    );

    let outcome = dispatcher.dispatch(envelope).await;

    assert_eq!(outcome.matched, Match::Matched);
    outcome.result.unwrap();
}

/// The README's second test, verbatim: the meta a handler over `Event<P>`
/// receives is what the payload carries.
#[tokio::test]
async fn the_installation_reaches_the_handler_from_the_payload() {
    let seen = Arc::new(Mutex::new(None));
    let dispatcher = Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], {
            let seen = Arc::clone(&seen);
            move |Event { meta, payload }: Event<IssueOpened>| {
                let seen = Arc::clone(&seen);
                async move {
                    *seen.lock().unwrap() = Some((meta.installation_id, payload.issue.number));
                    Ok::<_, AppError>(())
                }
            }
        })
        .build();

    let envelope = Envelope::new(
        "delivery-2",
        EventKind::Issues,
        br#"{"action":"opened","installation":{"id":42},"issue":{"number":7,"title":"Add tests"}}"#,
    );

    dispatcher.dispatch(envelope).await.result.unwrap();
    assert_eq!(*seen.lock().unwrap(), Some((Some(42), 7)));
}

/// The README's third test, verbatim: the body is a `String`, so the request
/// needs `http` and nothing from axum.
#[tokio::test]
async fn accepts_a_signed_delivery() {
    let dispatcher = Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], label)
        .build();
    let webhook =
        WebhookReceiverBuilder::new(Verifier::new(Secret::new("test-secret"))).build(dispatcher);

    let body = r#"{"action":"opened","issue":{"number":7,"title":"Add tests"}}"#;
    let request = http::Request::builder()
        .method("POST")
        .uri("/webhook")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::DELIVERY_ID, "delivery-1")
        .header(header::EVENT_NAME, "issues")
        .header(header::SIGNATURE, sign("test-secret", body.as_bytes()))
        .body(body.to_string())
        .unwrap();

    let response = webhook.receive(request).await;

    assert_eq!(response.status(), 204);
}

/// The README test's dispatcher, which the prose states its two unmatched
/// cases against.
fn labels_opened_issues() -> Dispatcher<AppError> {
    Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], label)
        .build()
}

/// The prose's aside: `EventMeta::new` on its own reads no bytes, where
/// `Envelope::new` over the same payload reads the action that routes it.
#[test]
fn a_meta_built_on_its_own_reads_no_bytes() {
    let payload = br#"{"action":"opened","installation":{"id":42},"issue":{"number":7}}"#;

    let meta = EventMeta::new("delivery-1", EventKind::Issues);
    let probed = Envelope::new("delivery-1", EventKind::Issues, payload);

    assert_eq!(meta.action, None);
    assert_eq!(meta.installation_id, None);
    assert_eq!(probed.meta.action, Some(Action::Opened));
    assert_eq!(probed.meta.installation_id, Some(42));
}

/// The first unmatched variant the prose names: the kind is registered, the
/// action is not, and nothing ran.
#[tokio::test]
async fn a_payload_without_an_action_is_unmatched_by_action() {
    let envelope = Envelope::new(
        "delivery-1",
        EventKind::Issues,
        br#"{"issue":{"number":7,"title":"Add tests"}}"#,
    );

    let outcome = labels_opened_issues().dispatch(envelope).await;

    assert_eq!(outcome.matched, Match::UnmatchedAction);
    outcome.result.unwrap();
}

/// The other unmatched variant the prose names: a kind the route table never
/// registered.
#[tokio::test]
async fn a_kind_the_route_table_does_not_know_is_unmatched_by_kind() {
    let envelope = Envelope::new("delivery-1", EventKind::Push, b"{}");

    let outcome = labels_opened_issues().dispatch(envelope).await;

    assert_eq!(outcome.matched, Match::UnmatchedKind);
    outcome.result.unwrap();
}

/// The hello world's observer walks the chain from `error.source.source()`,
/// one level below where `report` starts, because `Box<dyn Error + Send +
/// Sync>` is not itself an `Error` and `DispatchError` over it has no
/// `source()`. That walk is what reaches the serde field a decode failure
/// names; `error.source` alone displays the fieldless reason. The box is a
/// trait object, so `source()` on it needs no `use std::error::Error`.
#[tokio::test]
async fn the_hello_world_observer_reaches_the_serde_field_from_error_source_source() {
    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    async fn label(issue: IssueOpened) -> Result<(), BoxError> {
        println!("label #{}", issue.issue.number);
        Ok(())
    }

    let dispatcher = Dispatcher::<BoxError>::builder()
        .on_payload_action([Action::Opened], label)
        .build();

    let envelope = Envelope::new(
        "delivery-1",
        EventKind::Issues,
        br#"{"action":"opened","issue":{"number":7}}"#,
    );

    let error: DispatchError<BoxError> = dispatcher.dispatch(envelope).await.result.unwrap_err();

    // The hello world's observer, its `eprintln!`s collected instead.
    let mut lines = vec![format!("{error}: {}", error.source)];
    let mut cause = error.source.source();
    while let Some(error) = cause {
        lines.push(format!("  caused by: {error}"));
        cause = error.source();
    }

    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(
        lines[0].ends_with(": payload could not be decoded"),
        "{}",
        lines[0]
    );
    assert!(lines[1].contains("missing field `title`"), "{}", lines[1]);
}
