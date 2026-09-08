//! The README's `ignore` blocks, compiled.
//!
//! The README's Rust blocks are doctests, but the tests under "Testing
//! without GitHub" continue the quickstart and are marked `ignore`, so
//! nothing else compiles them. This file is those tests as written, with the
//! quickstart's `BoxError` and `thank` beside them, and holds the README's
//! prose to what it claims: `Envelope::new` reads the action out of the bytes
//! while `EventMeta::new` on its own reads none, an `http::Request<String>` is
//! a request `receive` accepts with no axum in sight, a receiver over another
//! secret answers the signed request 401, a verifier that also accepts a
//! previous secret signs under its first, a delivery nothing routes reports
//! which unmatched `Match` variant the Outcome table names, and the "Boxed
//! errors" observer reaches a decode failure's serde message from
//! `error.source.source()`.

#![cfg(all(feature = "http", not(target_arch = "wasm32")))]

use octoevents::{
    Action, DispatchError, Dispatcher, Envelope, EventKind, EventMeta, Match, Secret, Verifier,
    WebhookReceiverBuilder, header,
};

/// The quickstart's application error: no error enum, `?` converts anything.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The quickstart's handler, verbatim.
async fn thank(envelope: Envelope) -> Result<(), BoxError> {
    let sender = envelope.meta.sender.unwrap_or_default();
    println!("Thank you for your contribution, @{sender}! :)");
    Ok(())
}

/// The quickstart's dispatcher, which the README's tests and prose state
/// their claims against.
fn thanks_opened_issues() -> Dispatcher<BoxError> {
    Dispatcher::<BoxError>::builder()
        .on((EventKind::Issues, Action::Opened), thank)
        .build()
}

/// The README's first test, verbatim.
#[tokio::test]
async fn thanks_for_an_opened_issue() {
    let dispatcher = Dispatcher::<BoxError>::builder()
        .on((EventKind::Issues, Action::Opened), thank)
        .build();

    let envelope = Envelope::new(
        "delivery-1",
        EventKind::Issues,
        br#"{"action":"opened","sender":{"login":"octocat"}}"#,
    );

    let outcome = dispatcher.dispatch(envelope).await;

    assert_eq!(outcome.matched, Match::Matched);
    outcome.result.unwrap();
}

/// The README's second test, verbatim: the body is a `String`, so the
/// request needs `http` and nothing from axum, and the verifier the receiver
/// is built with signs it, so the test needs no HMAC code of its own.
#[tokio::test]
async fn accepts_a_signed_delivery() {
    let dispatcher = Dispatcher::<BoxError>::builder()
        .on((EventKind::Issues, Action::Opened), thank)
        .build();
    let verifier = Verifier::new(Secret::new("test-secret"));
    let webhook = WebhookReceiverBuilder::new(verifier.clone()).build(dispatcher);

    let body = r#"{"action":"opened","sender":{"login":"octocat"}}"#;
    let request = http::Request::builder()
        .method("POST")
        .uri("/webhook")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::DELIVERY_ID, "delivery-1")
        .header(header::EVENT_NAME, "issues")
        .header(header::SIGNATURE, verifier.sign(body.as_bytes()))
        .body(body.to_string())
        .unwrap();

    let response = webhook.receive(request).await;

    assert_eq!(response.status(), 204);
}

/// The sentence after the second test: the receiver built over another
/// secret answers the same request 401.
#[tokio::test]
async fn refuses_the_same_delivery_under_another_secret() {
    let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new("other-secret")))
        .build(thanks_opened_issues());

    let body = r#"{"action":"opened","sender":{"login":"octocat"}}"#;
    let request = http::Request::builder()
        .method("POST")
        .uri("/webhook")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::DELIVERY_ID, "delivery-1")
        .header(header::EVENT_NAME, "issues")
        .header(
            header::SIGNATURE,
            Verifier::new(Secret::new("test-secret")).sign(body.as_bytes()),
        )
        .body(body.to_string())
        .unwrap();

    let response = webhook.receive(request).await;

    assert_eq!(response.status(), 401);
}

/// The sentence's other half: a verifier that also accepts a previous secret
/// signs under its first, so a receiver over the previous secret alone
/// refuses what it signs, and one over the first accepts it.
#[tokio::test]
async fn a_verifier_with_a_previous_secret_signs_under_its_first() {
    let rotated = Verifier::new(Secret::new("test-secret")).also(Secret::new("previous-secret"));
    let body = r#"{"action":"opened","sender":{"login":"octocat"}}"#;
    let request = || {
        http::Request::builder()
            .method("POST")
            .uri("/webhook")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::DELIVERY_ID, "delivery-1")
            .header(header::EVENT_NAME, "issues")
            .header(header::SIGNATURE, rotated.sign(body.as_bytes()))
            .body(body.to_string())
            .unwrap()
    };

    let over_the_first = WebhookReceiverBuilder::new(Verifier::new(Secret::new("test-secret")))
        .build(thanks_opened_issues());
    let over_the_previous =
        WebhookReceiverBuilder::new(Verifier::new(Secret::new("previous-secret")))
            .build(thanks_opened_issues());

    assert_eq!(over_the_first.receive(request()).await.status(), 204);
    assert_eq!(over_the_previous.receive(request()).await.status(), 401);
}

/// The prose's claim: `Envelope::new` reads the action and the sender out of
/// the bytes the way the receiver does, where `EventMeta::new` on its own
/// reads none.
#[test]
fn an_envelope_reads_its_meta_from_the_bytes() {
    let payload = br#"{"action":"opened","installation":{"id":42},"sender":{"login":"octocat"}}"#;

    let meta = EventMeta::new("delivery-1", EventKind::Issues);
    let probed = Envelope::new("delivery-1", EventKind::Issues, payload);

    assert_eq!(meta.action, None);
    assert_eq!(meta.installation_id, None);
    assert_eq!(meta.sender, None);
    assert_eq!(probed.meta.action, Some(Action::Opened));
    assert_eq!(probed.meta.installation_id, Some(42));
    assert_eq!(probed.meta.sender.as_deref(), Some("octocat"));
}

/// The Outcome table's first unmatched row: the kind is registered, the
/// action is not, and nothing ran.
#[tokio::test]
async fn a_payload_without_an_action_is_unmatched_by_action() {
    let envelope = Envelope::new(
        "delivery-1",
        EventKind::Issues,
        br#"{"sender":{"login":"octocat"}}"#,
    );

    let outcome = thanks_opened_issues().dispatch(envelope).await;

    assert_eq!(outcome.matched, Match::UnmatchedAction);
    outcome.result.unwrap();
}

/// The Outcome table's other unmatched row: a kind the route table never
/// registered.
#[tokio::test]
async fn a_kind_the_route_table_does_not_know_is_unmatched_by_kind() {
    let envelope = Envelope::new("delivery-1", EventKind::Push, b"{}");

    let outcome = thanks_opened_issues().dispatch(envelope).await;

    assert_eq!(outcome.matched, Match::UnmatchedKind);
    outcome.result.unwrap();
}

/// The "Boxed errors" observer walks the chain from `error.source.source()`,
/// one level below where `report` starts, because `Box<dyn Error + Send +
/// Sync>` is not itself an `Error` and `DispatchError` over it has no
/// `source()`. That walk is what reaches the serde field a decode failure
/// names; `error.source` alone displays the fieldless reason. The box is a
/// trait object, so `source()` on it needs no `use std::error::Error`.
#[tokio::test]
async fn the_boxed_observer_reaches_the_serde_field_from_error_source_source() {
    /// A view whose `title` the payload below lacks; nothing reads it, the
    /// decode is the point. The README derives its kind; here the impl is
    /// written by hand so the file compiles without the `derive` feature.
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct IssueOpened {
        issue: Issue,
    }
    impl octoevents::Payload for IssueOpened {
        const KIND: EventKind = EventKind::Issues;
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Issue {
        title: String,
    }

    async fn label(_: IssueOpened) -> Result<(), BoxError> {
        Ok(())
    }

    let dispatcher = Dispatcher::<BoxError>::builder()
        .on([Action::Opened], label)
        .build();

    let envelope = Envelope::new(
        "delivery-1",
        EventKind::Issues,
        br#"{"action":"opened","issue":{"number":7}}"#,
    );

    let error: DispatchError<BoxError> = dispatcher.dispatch(envelope).await.result.unwrap_err();

    // The README's observer, its `eprintln!`s collected instead.
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
