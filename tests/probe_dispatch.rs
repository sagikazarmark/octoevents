//! Signed payload shapes and field isolation through action-specific dispatch.

#![cfg(not(target_arch = "wasm32"))]

use std::{convert::Infallible, sync::Arc};

use bytes::Bytes;
use http::{Request, StatusCode};
use octoevents::{
    Action, Dispatcher, Envelope, EventKind, EventMeta, Verifier, WebhookReceiverBuilder,
    WebhookSecret,
};
use tokio::sync::Mutex;

#[tokio::test]
async fn signed_dispatch_routes_only_an_unambiguous_action_from_a_payload_object() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let routed = Arc::clone(&received);
    let fallback = Arc::clone(&received);
    let dispatcher = Dispatcher::builder()
        .on(
            (EventKind::Issues, Action::Opened),
            move |meta: EventMeta| {
                let routed = Arc::clone(&routed);
                async move {
                    routed.lock().await.push(("route", meta));
                    Ok::<_, Infallible>(())
                }
            },
        )
        .fallback(move |envelope: Envelope| {
            let fallback = Arc::clone(&fallback);
            async move {
                fallback.lock().await.push(("fallback", envelope.meta));
                Ok::<_, Infallible>(())
            }
        })
        .build();
    let verifier = Verifier::new(WebhookSecret::new("development-secret"));
    let webhook = WebhookReceiverBuilder::new(verifier.clone()).build(dispatcher);

    for (payload, tier, action) in [
        (
            r#"["opened",{"id":42},null,null,{"id":2,"login":"monalisa"}]"#,
            "fallback",
            None,
        ),
        (r#"{"action":"opened","action":"opened"}"#, "fallback", None),
        (r#"{"action":"closed","action":"opened"}"#, "fallback", None),
        (
            r#"{"action":"opened","sender":[2,"monalisa"]}"#,
            "route",
            Some(Action::Opened),
        ),
        (
            r#"{"action":"opened","sender":{"id":2,"login":"monalisa"},"sender":{"id":3,"login":"octo"}}"#,
            "route",
            Some(Action::Opened),
        ),
        (r#"{"action":"opened"}"#, "route", Some(Action::Opened)),
    ] {
        received.lock().await.clear();
        let request = Request::builder()
            .header("content-type", "application/json")
            .header("x-github-delivery", "delivery")
            .header("x-github-event", "issues")
            .header("x-hub-signature-256", verifier.sign(payload.as_bytes()))
            .body(())
            .unwrap();
        let response = webhook
            .receive_bytes(
                request.headers(),
                Bytes::copy_from_slice(payload.as_bytes()),
            )
            .await;

        assert_eq!(response, StatusCode::NO_CONTENT, "{payload}");
        let mut expected = EventMeta::new("delivery", EventKind::Issues);
        expected.action = action;
        assert_eq!(
            received.lock().await.as_slice(),
            [(tier, expected)],
            "{payload}"
        );
    }
}
