//! Minimal Tower service mounted on an Axum route.

use std::convert::Infallible;

use axum::{Router, routing::post_service};
use octoevents::{Envelope, Secret, Verifier, WebhookReceiverBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;
    let verifier = Verifier::new(Secret::new(secret));
    let webhook = WebhookReceiverBuilder::new(verifier).build(|envelope: Envelope| async move {
        println!("received {} ({})", envelope.delivery_id, envelope.kind);
        Ok::<_, Infallible>(())
    });

    let app: Router = Router::new().route("/webhook", post_service(webhook));
    let address = std::env::var("WEBHOOK_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("listening on http://{address}/webhook");
    axum::serve(listener, app).await?;

    Ok(())
}
