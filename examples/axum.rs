//! The simplest shape: a receiver around one handler over the envelope, with
//! no dispatcher, mounted on an axum route as a Tower service.
//!
//! The quickstart routes through a dispatcher; this example shows that the
//! receiver needs none. `WebhookReceiverBuilder::build` takes any
//! `Handler<Envelope>`, and a struct with dependencies is the usual one. The
//! next rungs are `dispatcher` (every tier and every handler input) and
//! `policy_seam` (persistence, deduplication and dead-lettering around a
//! dispatcher).
//!
//! Run with real deliveries forwarded by `gh webhook forward` (see the README):
//!
//! ```console
//! GITHUB_WEBHOOK_SECRET=development-secret cargo run --example axum --features tower
//! ```
//!
//! `WEBHOOK_ADDRESS` overrides the address it listens on, `127.0.0.1:3000` by
//! default, which is what the README's `gh webhook forward` line targets.
//! The tests at the bottom drive the receiver with a request signed by
//! `Verifier::sign`; run them with `cargo test --example axum --features tower`.

use axum::{Router, routing::post_service};
use octoevents::{Envelope, Handler, Verifier, WebhookReceiverBuilder, WebhookSecret};

/// The application error the handler returns; the receiver answers it with a
/// 500. A real one wraps what the handler's dependencies fail with.
#[derive(Debug, thiserror::Error)]
#[error("the delivery could not be announced")]
struct AppError;

/// A handler is a struct whose fields are its dependencies. This one has
/// none yet; a database pool or API client would go here and be borrowed
/// through `&self` on every delivery.
struct Announce;

impl Handler<Envelope> for Announce {
    type Error = AppError;

    // The handler prints where a real one would await its dependencies. The
    // lint expectation says so; delete it once the body has an `.await`.
    #[expect(clippy::unused_async_trait_impl)]
    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        println!(
            "received {} ({})",
            envelope.meta.delivery_id, envelope.meta.kind
        );
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;
    let verifier = Verifier::new(WebhookSecret::new(secret));
    let webhook = WebhookReceiverBuilder::new(verifier).build(Announce);

    let app: Router = Router::new().route("/webhook", post_service(webhook));
    let address = std::env::var("WEBHOOK_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("listening on http://{address}/webhook");
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use octoevents::header;

    use super::*;

    /// A signed request for `body`, with the four headers every delivery
    /// needs, as GitHub would send it under `verifier`'s first secret.
    fn signed_request(verifier: &Verifier, body: &str) -> http::Request<String> {
        http::Request::builder()
            .method("POST")
            .uri("/webhook")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::DELIVERY_ID, "delivery-1")
            .header(header::EVENT_NAME, "push")
            .header(header::SIGNATURE, verifier.sign(body.as_bytes()))
            .body(body.to_string())
            .unwrap()
    }

    /// The handler runs for a verified delivery and the receiver answers 204.
    #[tokio::test]
    async fn announces_a_signed_delivery() {
        let verifier = Verifier::new(WebhookSecret::new("test-secret"));
        let webhook = WebhookReceiverBuilder::new(verifier.clone()).build(Announce);

        let response = webhook
            .receive(signed_request(&verifier, r#"{"ref":"refs/heads/main"}"#))
            .await;

        assert_eq!(response.status(), 204);
    }

    /// A request signed under another secret never reaches the handler.
    #[tokio::test]
    async fn refuses_a_delivery_signed_with_another_secret() {
        let webhook = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("test-secret")))
            .build(Announce);
        let other = Verifier::new(WebhookSecret::new("other-secret"));

        let response = webhook
            .receive(signed_request(&other, r#"{"ref":"refs/heads/main"}"#))
            .await;

        assert_eq!(response.status(), 401);
    }
}
