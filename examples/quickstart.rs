//! Start here: the README's quickstart, verbatim, with its tests.
//!
//! A receiver that thanks the author of every opened issue. One `async fn`
//! handler over the [`Envelope`], routed by kind and action through a
//! dispatcher, behind a receiver that verifies every request and mounts on an
//! axum route. The next rungs are `axum` (a receiver with no dispatcher, the
//! simplest shape), `dispatcher` (every tier and every handler input) and
//! `policy_seam` (persistence, deduplication and dead-lettering around a
//! dispatcher).
//!
//! Run it with real deliveries forwarded by `gh webhook forward` (the README's
//! "Try it" section walks through it):
//!
//! ```console
//! GITHUB_WEBHOOK_SECRET=development-secret cargo run --example quickstart --features tower
//! ```
//!
//! The tests at the bottom are the README's "Testing without GitHub" tests:
//! the dispatcher is driven with an envelope from [`Envelope::new`], nothing
//! signed, and the receiver with a request signed by [`Verifier::sign`].
//! Run them with `cargo test --example quickstart --features tower`.

use axum::{Router, routing::post_service};
use octoevents::{
    Action, BoxError, Dispatcher, Envelope, EventKind, Verifier, WebhookReceiverBuilder,
    WebhookSecret,
};

/// Runs for `issues.opened`. The envelope is the verified unit of receipt: its
/// meta (delivery ID, kind, action, repository, sender, ...) and the raw payload.
/// `BoxError` is the crate's erased error; any `Error + Send + Sync + 'static` converts into it with `?`.
async fn thank(envelope: Envelope) -> Result<(), BoxError> {
    let sender = envelope.meta.sender.map(|s| s.login).unwrap_or_default();
    println!("Thank you for your contribution, @{sender}! :)");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;

    // Routes each verified envelope by kind and action.
    let dispatcher = Dispatcher::builder()
        .on((EventKind::Issues, Action::Opened), thank)
        .build();

    // Verifies `X-Hub-Signature-256` against the body with this secret.
    // A receiver cannot be built without one.
    let verifier = Verifier::new(WebhookSecret::new(secret));

    // Refuses what does not verify; hands everything else to the dispatcher.
    let webhook = WebhookReceiverBuilder::new(verifier).build(dispatcher);

    // The receiver owns no paths or methods: mount it on your router.
    let app = Router::new().route("/webhook", post_service(webhook));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use octoevents::{Match, header};

    use super::*;

    /// The handler is tested through `dispatch` with an envelope from
    /// `Envelope::new`: nothing is signed, because nothing is verified on
    /// this path, and the meta is read from the bytes the way the receiver
    /// reads it.
    #[tokio::test]
    async fn thanks_for_an_opened_issue() {
        let dispatcher = Dispatcher::builder()
            .on((EventKind::Issues, Action::Opened), thank)
            .build();

        let envelope = Envelope::new(
            "delivery-1",
            EventKind::Issues,
            br#"{"action":"opened","sender":{"id":1,"login":"octocat"}}"#,
        );

        let outcome = dispatcher.dispatch(envelope).await;

        assert_eq!(outcome.matched, Match::Matched);
        outcome.result.unwrap();
    }

    /// The receiver is tested with a signed synthetic request: the verifier
    /// the receiver is built with signs the body, and the request needs no
    /// server and no axum, since a `String` body is one `receive` accepts.
    #[tokio::test]
    async fn accepts_a_signed_delivery() {
        let dispatcher = Dispatcher::builder()
            .on((EventKind::Issues, Action::Opened), thank)
            .build();
        let verifier = Verifier::new(WebhookSecret::new("test-secret"));
        let webhook = WebhookReceiverBuilder::new(verifier.clone()).build(dispatcher);

        let body = r#"{"action":"opened","sender":{"id":1,"login":"octocat"}}"#;
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

    /// Build the receiver over another secret and the same request is
    /// answered 401.
    #[tokio::test]
    async fn refuses_a_delivery_signed_with_another_secret() {
        let dispatcher = Dispatcher::builder()
            .on((EventKind::Issues, Action::Opened), thank)
            .build();
        let webhook =
            WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("other-secret")))
                .build(dispatcher);
        let signer = Verifier::new(WebhookSecret::new("test-secret"));

        let body = r#"{"action":"opened","sender":{"id":1,"login":"octocat"}}"#;
        let request = http::Request::builder()
            .method("POST")
            .uri("/webhook")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::DELIVERY_ID, "delivery-1")
            .header(header::EVENT_NAME, "issues")
            .header(header::SIGNATURE, signer.sign(body.as_bytes()))
            .body(body.to_string())
            .unwrap();

        let response = webhook.receive(request).await;

        assert_eq!(response.status(), 401);
    }
}
