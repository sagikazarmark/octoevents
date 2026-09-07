//! `receive` composes with axum's `post` for every handler `post_service`
//! accepts.
//!
//! The README's hello world names `Box<dyn Error + Send + Sync>` as the
//! application error, and its "One event, one handler" program mounts the
//! receiver without the `tower` feature by calling `receive` inside an axum
//! closure. axum requires that closure's future to be `Send`, so the two
//! compose only if `receive`'s future is `Send` for a dispatcher over the
//! boxed error. These tests hold the receiver to that: a bare `Send`
//! assertion on the future, and the README's wiring driven end to end with
//! the README's hello world as the handler.

#![cfg(all(feature = "http", not(target_arch = "wasm32")))]

mod common;

use axum::{Router, body::Body, extract::Request, routing::post};
use bytes::Bytes;
use http::StatusCode;
use octoevents::{
    DispatchError, Dispatcher, Envelope, Secret, Verifier, WebhookReceiver, WebhookReceiverBuilder,
};
use tower::ServiceExt as _;

/// The README's application error: no error enum, `?` converts anything.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

const SECRET: &str = "development-secret";

/// The README's hello world handler: an `async fn` item over the envelope.
async fn print(envelope: Envelope) -> Result<(), BoxError> {
    println!("{} {}", envelope.meta.delivery_id, envelope.meta.kind);
    Ok(())
}

/// The README's hello world receiver, verbatim: a dispatcher over the boxed
/// error with `print` in its always tier, and an observer that says why a
/// delivery failed.
fn hello_world() -> WebhookReceiver<Dispatcher<BoxError>> {
    let dispatcher = Dispatcher::<BoxError>::builder().always(print).build();
    WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .on_error(|_, error: &DispatchError<BoxError>| {
            eprintln!("{error}: {}", error.source);
            let mut cause = error.source.source();
            while let Some(error) = cause {
                eprintln!("  caused by: {error}");
                cause = error.source();
            }
        })
        .build(dispatcher)
}

/// A signed request for `event` carrying `body`, as GitHub would send it.
fn signed(event: &str, body: &'static [u8]) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("content-type", "application/json")
        .header("x-github-delivery", "delivery")
        .header("x-github-event", event)
        .header(
            "x-hub-signature-256",
            common::signature(SECRET.as_bytes(), body),
        )
        .body(Body::from(Bytes::from_static(body)))
        .unwrap()
}

#[test]
fn the_receive_future_is_send_for_a_dispatcher_over_a_boxed_error() {
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
