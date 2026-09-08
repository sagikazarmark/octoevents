//! `receive` composes with axum's `post` for every handler `post_service`
//! accepts.
//!
//! The README's quickstart names `Box<dyn Error + Send + Sync>` as the
//! application error, and its "Transports" section mounts the receiver
//! without the `tower` feature by calling `receive` inside an axum closure.
//! axum requires that closure's future to be `Send`, so the two compose only
//! if `receive`'s future is `Send` for a dispatcher over the boxed error.
//! These tests hold the receiver to that: a bare `Send` assertion on the
//! future, and the README's wiring driven end to end with the README's
//! quickstart handler.

#![cfg(all(feature = "http", not(target_arch = "wasm32")))]

use axum::{Router, body::Body, extract::Request, routing::post};
use bytes::Bytes;
use http::StatusCode;
use octoevents::{
    Action, Dispatcher, Envelope, EventKind, Verifier, WebhookReceiver, WebhookReceiverBuilder,
    WebhookSecret,
};
use tower::ServiceExt as _;

/// The README's application error: no error enum, `?` converts anything.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The README's quickstart verifier, over its development secret; the
/// requests below are signed with it.
fn verifier() -> Verifier {
    Verifier::new(WebhookSecret::new("development-secret"))
}

/// The README's quickstart handler: an `async fn` item over the envelope.
async fn thank(envelope: Envelope) -> Result<(), BoxError> {
    let sender = envelope.meta.sender.unwrap_or_default();
    println!("Thank you for your contribution, @{sender}! :)");
    Ok(())
}

/// The README's quickstart receiver: a dispatcher over the boxed error with
/// `thank` routed for `issues.opened`.
fn quickstart() -> WebhookReceiver<Dispatcher<BoxError>> {
    let dispatcher = Dispatcher::<BoxError>::builder()
        .on((EventKind::Issues, Action::Opened), thank)
        .build();
    WebhookReceiverBuilder::new(verifier()).build(dispatcher)
}

/// A signed request for `event` carrying `body`, as GitHub would send it.
fn signed(event: &str, body: &'static [u8]) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("content-type", "application/json")
        .header("x-github-delivery", "delivery")
        .header("x-github-event", event)
        .header("x-hub-signature-256", verifier().sign(body).to_string())
        .body(Body::from(Bytes::from_static(body)))
        .unwrap()
}

#[test]
fn the_receive_future_is_send_for_a_dispatcher_over_a_boxed_error() {
    fn assert_send<T: Send>(_: T) {}

    let webhook = quickstart();

    assert_send(webhook.receive(signed("push", b"{}")));
}

#[tokio::test]
async fn the_readme_wiring_without_tower_accepts_a_dispatcher_over_a_boxed_error() {
    let webhook = quickstart();

    // The README's "Transports" wiring, verbatim: a plain axum handler
    // calling `receive`, no `post_service`.
    let app: Router = Router::new().route(
        "/webhook",
        post(move |request: Request| {
            let webhook = webhook.clone();
            async move { webhook.receive(request).await }
        }),
    );

    let response = app
        .oneshot(signed(
            "issues",
            br#"{"action":"opened","sender":{"login":"octocat"}}"#,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}
