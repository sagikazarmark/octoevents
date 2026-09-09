//! `#[derive(Payload)]` against the trait it implements.
//!
//! The derive's own crate checks what the expansion says; this file checks
//! what it does: the kind is the declared one, an envelope of that kind
//! decodes into the view and one of another kind is a mismatch, `Event<P>`
//! over the view is a payload of the same kind, and a generic view is a
//! payload wherever it deserializes, with nothing said about its parameter
//! beyond what the type itself declares. That last one is the point of the
//! `Self: DeserializeOwned` bound the derive adds: `Payload` requires
//! `FromEnvelope`, which a serde type has only through `DeserializeOwned`, so
//! an unbounded `View<T>` would owe it for every `T` and be refused.
//!
//! Gated on the feature that provides the derive; the rest of the suite
//! writes its `impl Payload` by hand so `cargo test --no-default-features`
//! compiles it.

#![cfg(feature = "derive")]

use std::convert::Infallible;

use octoevents::{
    AnyAction, DecodeError, Dispatcher, Envelope, Event, EventKind, FromEnvelope as _, Payload,
};

#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::Issues)]
struct IssueNumber {
    issue: Issue,
}

#[derive(serde::Deserialize)]
struct Issue {
    number: u64,
}

/// A view generic over the shape of one field, with no bound on `T`.
#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::PullRequest)]
struct PullRequest<T> {
    pull_request: T,
}

/// The shape a `PullRequest<T>` reads its field as in the tests below.
#[derive(Debug, PartialEq, serde::Deserialize)]
struct Number {
    number: u64,
}

/// A view with bounds and a where clause of its own, which the derive keeps
/// beside the one it adds.
#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::Push)]
struct Ref<T: Clone>
where
    T: Send,
{
    r#ref: T,
}

#[test]
fn the_declared_kind_is_the_payloads_kind() {
    assert_eq!(IssueNumber::KIND, EventKind::Issues);
    assert_eq!(<Event<IssueNumber>>::KIND, EventKind::Issues);
    assert_eq!(<PullRequest<Number>>::KIND, EventKind::PullRequest);
    assert_eq!(<Ref<String>>::KIND, EventKind::Push);
}

#[test]
fn an_envelope_of_the_kind_decodes_into_the_view() {
    let envelope = Envelope::new(
        "delivery",
        EventKind::Issues,
        br#"{"action":"opened","issue":{"number":7,"title":"unread"}}"#,
    );

    let view = IssueNumber::from_envelope(&envelope).unwrap();
    assert_eq!(view.issue.number, 7);

    let event = Event::<IssueNumber>::from_envelope(&envelope).unwrap();
    assert_eq!(event.meta.delivery_id, "delivery");
    assert_eq!(event.payload.issue.number, 7);
}

#[test]
fn an_envelope_of_another_kind_is_a_kind_mismatch() {
    let envelope = Envelope::new(
        "delivery",
        EventKind::PullRequest,
        br#"{"issue":{"number":7}}"#,
    );

    let Err(error) = IssueNumber::from_envelope(&envelope) else {
        panic!("a pull_request envelope decoded as an issues view");
    };
    assert!(
        matches!(
            &error,
            DecodeError::KindMismatch { expected, actual }
                if *expected == EventKind::Issues && *actual == EventKind::PullRequest
        ),
        "{error}"
    );
}

#[test]
fn a_generic_view_decodes_as_each_type_its_field_deserializes_as() {
    let envelope = Envelope::new(
        "delivery",
        EventKind::PullRequest,
        br#"{"action":"opened","pull_request":{"number":7}}"#,
    );

    let numbered = PullRequest::<Number>::from_envelope(&envelope).unwrap();
    assert_eq!(numbered.pull_request, Number { number: 7 });

    let untyped = PullRequest::<serde_json::Value>::from_envelope(&envelope).unwrap();
    assert_eq!(untyped.pull_request["number"], 7);

    let push = Envelope::new("delivery", EventKind::Push, br#"{"ref":"refs/heads/main"}"#);
    let named = Ref::<String>::from_envelope(&push).unwrap();
    assert_eq!(named.r#ref, "refs/heads/main");
}

#[test]
fn a_handler_over_a_generic_view_is_registered_by_the_views_kind() {
    async fn number(pr: PullRequest<Number>) -> Result<(), Infallible> {
        println!("PR #{}", pr.pull_request.number);
        Ok(())
    }

    async fn untyped(
        Event { payload, .. }: Event<PullRequest<serde_json::Value>>,
    ) -> Result<(), Infallible> {
        println!("{}", payload.pull_request);
        Ok(())
    }

    let _dispatcher = Dispatcher::builder()
        .on(AnyAction, number)
        .on(AnyAction, untyped)
        .build();
}
