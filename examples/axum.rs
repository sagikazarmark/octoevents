//! Minimal Tower service mounted on an Axum route.

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

    // A real handler awaits its dependencies here.
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
