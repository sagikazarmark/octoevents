//! `receive` composes with axum's `post` for every handler `post_service`
//! accepts.
//!
//! The README's hello world names `Box<dyn Error + Send + Sync>` as the
//! application error, and its "One event, one handler" program mounts the
//! receiver without the `tower` feature by calling `receive` inside an axum
//! closure. axum requires that closure's future to be `Send`, so the two
//! compose only if `receive`'s future is `Send` for a dispatcher over the
//! boxed error. These tests hold the receiver to that: a bare `Send`
//! assertion on the future, and the README's wiring driven end to end.

#![cfg(all(feature = "http", not(target_arch = "wasm32")))]

use std::error::Error;

use axum::{Router, body::Body, extract::Request, routing::post};
use bytes::Bytes;
use hmac::{Hmac, KeyInit, Mac};
use http::StatusCode;
use octoevents::{Dispatcher, Envelope, Secret, Verifier, WebhookReceiver, WebhookReceiverBuilder};
use sha2::Sha256;
use tower::ServiceExt as _;

/// The README's application error: no error enum, `?` converts anything.
type BoxError = Box<dyn Error + Send + Sync>;

const SECRET: &str = "development-secret";

/// The README's hello world receiver: a dispatcher over the boxed error whose
/// always tier sees every verified delivery.
fn hello_world() -> WebhookReceiver<Dispatcher<BoxError>> {
    let dispatcher = Dispatcher::<BoxError>::builder()
        .always(|_: Envelope| async { Ok::<(), BoxError>(()) })
        .build();
    WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET))).build(dispatcher)
}

/// The `X-Hub-Signature-256` value GitHub would send for `body` under `secret`.
fn signature(secret: &[u8], body: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    let mut out = String::from("sha256=");
    for byte in tag {
        write!(out, "{byte:02x}").unwrap();
    }
    out
}

/// A signed request for `event` carrying `body`, as GitHub would send it.
fn signed(event: &str, body: &'static [u8]) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("content-type", "application/json")
        .header("x-github-delivery", "delivery")
        .header("x-github-event", event)
        .header("x-hub-signature-256", signature(SECRET.as_bytes(), body))
        .body(Body::from(Bytes::from_static(body)))
        .unwrap()
}

#[test]
fn receives_future_is_send_for_a_dispatcher_over_a_boxed_error() {
    fn assert_send<T: Send>(_: T) {}

    let webhook = hello_world();

    assert_send(webhook.receive(signed("push", b"{}")));
}

#[tokio::test]
async fn the_readme_wiring_without_tower_accepts_a_dispatcher_over_a_boxed_error() {
    let webhook = hello_world();

    // The README's "One event, one handler" wiring, verbatim: a plain axum
    // handler calling `receive`, no `post_service`.
    let app = Router::new().route(
        "/webhook",
        post(move |request: Request| {
            let webhook = webhook.clone();
            async move { webhook.receive(request).await }
        }),
    );

    let response = app.oneshot(signed("push", b"{}")).await.unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}
