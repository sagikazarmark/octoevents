//! Invocation multiplicity and ordering through the public dispatcher seam.

#![cfg(not(target_arch = "wasm32"))]

use std::{convert::Infallible, sync::Arc};

use octoevents::{
    Action, Dispatcher, Envelope, EventKind, EventMatcher, EventMeta, Handler, Match,
};
use tokio::sync::Mutex;

type Calls = Arc<Mutex<Vec<&'static str>>>;

#[tokio::test]
async fn static_and_forwarded_wire_names_route_with_the_same_identity() {
    for (kind, action, payload) in [
        (
            EventKind::from_static("issues"),
            Action::from_static("opened"),
            br#"{"action":"opened"}"#.as_slice(),
        ),
        (
            EventKind::from_static("future_event"),
            Action::from_static("future_action"),
            br#"{"action":"future_action"}"#.as_slice(),
        ),
    ] {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on((kind.clone(), action), recording(&calls, "registered"))
            .build();
        let envelope = Envelope::new("delivery-1", EventKind::from(kind.to_string()), payload);
        let forwarded = serde_json::to_string(&envelope).unwrap();
        let received: Envelope = serde_json::from_str(&forwarded).unwrap();
        assert_eq!(received, envelope);

        let outcome = dispatcher.dispatch(received).await;
        assert_eq!(outcome.matched, Match::Matched);
        outcome.result.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["registered"]);
    }
}

fn recording(
    calls: &Calls,
    name: &'static str,
) -> impl Handler<EventMeta, Error = Infallible> + use<> {
    let calls = Arc::clone(calls);
    move |_: EventMeta| {
        let calls = Arc::clone(&calls);
        async move {
            calls.lock().await.push(name);
            Ok(())
        }
    }
}

#[tokio::test]
async fn duplicate_kinds_in_one_registration_run_once_per_delivery() {
    let calls = Calls::default();
    let dispatcher = Dispatcher::builder()
        .on(
            [EventKind::Push, EventKind::Issues, EventKind::Push],
            recording(&calls, "registered"),
        )
        .build();

    for kind in [EventKind::Push, EventKind::Issues, EventKind::Push] {
        calls.lock().await.clear();
        let outcome = dispatcher
            .dispatch(Envelope::new("delivery-1", kind, b"{}"))
            .await;
        assert_eq!(outcome.matched, Match::Matched);
        outcome.result.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["registered"]);
    }
}

#[tokio::test]
async fn duplicate_actions_in_one_registration_have_no_additional_effect() {
    let calls = Calls::default();
    let matcher = EventMatcher::from((
        EventKind::Issues,
        [Action::Opened, Action::Closed, Action::Opened],
    ))
    .or(vec![(EventKind::Issues, Action::Opened)]);
    let dispatcher = Dispatcher::builder()
        .on(matcher, recording(&calls, "registered"))
        .build();

    for payload in [
        br#"{"action":"opened"}"#.as_slice(),
        br#"{"action":"closed"}"#.as_slice(),
    ] {
        calls.lock().await.clear();
        let outcome = dispatcher
            .dispatch(Envelope::new("delivery-1", EventKind::Issues, payload))
            .await;
        assert_eq!(outcome.matched, Match::Matched);
        outcome.result.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["registered"]);
    }
}

#[tokio::test]
async fn overlapping_selections_run_at_the_first_applicable_position() {
    for matcher in [
        EventMatcher::from(EventKind::Issues).or((EventKind::Issues, Action::Opened)),
        EventMatcher::from((EventKind::Issues, Action::Opened)).or(EventKind::Issues),
    ] {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(EventKind::Issues, recording(&calls, "kind-before"))
            .on(
                (EventKind::Issues, Action::Opened),
                recording(&calls, "action-before"),
            )
            .on(matcher, recording(&calls, "overlap"))
            .on(
                (EventKind::Issues, Action::Opened),
                recording(&calls, "action-after"),
            )
            .on(EventKind::Issues, recording(&calls, "kind-after"))
            .fallback(|_: Envelope| async { Err::<(), _>("unexpected fallback") })
            .build();

        for (payload, expected) in [
            (
                br#"{"action":"opened"}"#.as_slice(),
                vec![
                    "action-before",
                    "overlap",
                    "action-after",
                    "kind-before",
                    "kind-after",
                ],
            ),
            (
                br#"{"action":"closed"}"#.as_slice(),
                vec!["kind-before", "overlap", "kind-after"],
            ),
            (
                b"{}".as_slice(),
                vec!["kind-before", "overlap", "kind-after"],
            ),
        ] {
            calls.lock().await.clear();
            let outcome = dispatcher
                .dispatch(Envelope::new("delivery-1", EventKind::Issues, payload))
                .await;
            assert_eq!(outcome.matched, Match::Matched);
            outcome.result.unwrap();
            assert_eq!(*calls.lock().await, expected);
        }
    }
}

#[tokio::test]
async fn shared_handler_registrations_at_the_same_source_location_remain_independent() {
    let calls = Calls::default();
    let handler = Arc::new(recording(&calls, "shared"));
    let mut builder = Dispatcher::builder();
    for _ in 0..2 {
        builder = builder.on(
            EventMatcher::from([EventKind::Issues, EventKind::Issues])
                .or((EventKind::Issues, [Action::Opened, Action::Opened])),
            Arc::clone(&handler),
        );
    }
    let dispatcher = builder.build();

    let outcome = dispatcher
        .dispatch(Envelope::new(
            "delivery-1",
            EventKind::Issues,
            br#"{"action":"opened"}"#,
        ))
        .await;
    assert_eq!(outcome.matched, Match::Matched);
    outcome.result.unwrap();
    assert_eq!(calls.lock().await.as_slice(), ["shared", "shared"]);
}

#[tokio::test]
async fn duplicate_relative_actions_run_the_payload_handler_once() {
    #[derive(serde::Deserialize)]
    struct IssueNumber {
        number: u64,
    }
    impl octoevents::Payload for IssueNumber {
        const KIND: EventKind = EventKind::Issues;
    }

    let numbers = Arc::new(Mutex::new(Vec::new()));
    let handler_numbers = Arc::clone(&numbers);
    let dispatcher = Dispatcher::builder()
        .on(
            [Action::Opened, Action::Opened],
            move |issue: IssueNumber| {
                let numbers = Arc::clone(&handler_numbers);
                async move {
                    numbers.lock().await.push(issue.number);
                    Ok::<_, Infallible>(())
                }
            },
        )
        .build();

    let outcome = dispatcher
        .dispatch(Envelope::new(
            "delivery-1",
            EventKind::Issues,
            br#"{"action":"opened","number":42}"#,
        ))
        .await;
    assert_eq!(outcome.matched, Match::Matched);
    outcome.result.unwrap();
    assert_eq!(numbers.lock().await.as_slice(), [42]);
}
