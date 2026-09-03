//! A GitHub App receiver built from struct handlers of every flavour.
//!
//! Run with real deliveries forwarded by `gh webhook forward` (see the README):
//!
//! ```console
//! GITHUB_WEBHOOK_SECRET=development-secret \
//!   cargo run --example dispatcher --features tower,octocrab
//! ```
//!
//! Three handler flavours appear, each a struct whose fields are its
//! dependencies and each with its own error type:
//!
//! - [`Persist`] is a `WebhookHandler`: it sees the raw envelope, stores it,
//!   and only then hands it to the dispatcher. Wrapping the dispatcher is how
//!   raw-bytes work runs before any typed handler.
//! - [`Auditor`] and [`Reject`] are `EventHandler`s over octocrab's decoded
//!   `WebhookEvent`; one runs for every delivery, one only when nothing
//!   matched.
//! - [`Labeler`] is a `PayloadHandler` over octocrab's pull-request payload;
//!   its kind comes from that type, so registering it needs no matcher.

// The handlers here print instead of awaiting a database or the GitHub API,
// which is what a real `async fn handle` would do.
#![allow(clippy::unused_async_trait_impl)]

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};

use axum::{Router, routing::post_service};
use octocrab::models::webhook_events::{WebhookEvent, payload::PullRequestWebhookEventPayload};
use octoevents::{
    Action, DecodeError, Dispatcher, Envelope, EventHandler, EventKind, EventMeta, PayloadHandler,
    Secret, Verifier, WebhookHandler, WebhookReceiverBuilder,
};

/// The application error every handler's error converts into.
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Label(#[from] LabelError),
    #[error("no handler for {kind} {action:?}")]
    Unhandled {
        kind: EventKind,
        action: Option<Action>,
    },
}

impl From<Infallible> for AppError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}

/// A stand-in for a database: remembers which deliveries were stored.
#[derive(Default)]
struct Store {
    delivery_ids: Mutex<Vec<String>>,
}

#[derive(Debug, thiserror::Error)]
#[error("delivery {0} was already stored")]
struct StoreError(String);

impl Store {
    fn insert(&self, delivery_id: &str) -> Result<(), StoreError> {
        let mut ids = self.delivery_ids.lock().expect("store lock");
        if ids.iter().any(|id| id == delivery_id) {
            return Err(StoreError(delivery_id.to_owned()));
        }
        ids.push(delivery_id.to_owned());
        Ok(())
    }
}

/// Persists the raw envelope, then routes it. The persist-before-handle
/// advice from the crate docs, expressed as a webhook handler that wraps the
/// dispatcher.
struct Persist {
    store: Arc<Store>,
    dispatcher: Dispatcher<AppError>,
}

impl WebhookHandler for Persist {
    type Error = AppError;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        self.store.insert(&envelope.meta.delivery_id)?;
        println!(
            "stored {} ({} bytes)",
            envelope.meta.delivery_id,
            envelope.raw.len()
        );
        self.dispatcher.handle(envelope).await
    }
}

/// Runs for every delivery. Its error type says it cannot fail.
struct Auditor;

impl EventHandler for Auditor {
    type Error = Infallible;

    async fn handle(&self, meta: EventMeta, event: WebhookEvent) -> Result<(), Self::Error> {
        println!(
            "audit {} {} {:?} from {}",
            meta.delivery_id,
            meta.kind,
            meta.action,
            event
                .sender
                .map_or_else(|| "unknown".into(), |sender| sender.login),
        );
        Ok(())
    }
}

/// Labels pull requests. Receives the decoded payload and no raw bytes.
struct Labeler {
    label: String, // stands in for a GitHub API client
}

#[derive(Debug, thiserror::Error)]
#[error("could not label pull request #{0}")]
struct LabelError(u64);

impl PayloadHandler<PullRequestWebhookEventPayload> for Labeler {
    type Error = LabelError;

    async fn handle(
        &self,
        meta: EventMeta,
        payload: PullRequestWebhookEventPayload,
    ) -> Result<(), Self::Error> {
        // Filter on the action inside the handler, with the payload's typed field.
        use octocrab::models::webhook_events::payload::PullRequestWebhookEventAction::Opened;
        if payload.action != Opened {
            return Ok(());
        }
        let installation = meta.installation_id.ok_or(LabelError(payload.number))?;
        println!(
            "label PR #{} '{}' as {} via installation {installation}",
            payload.number,
            payload.pull_request.title.unwrap_or_default(),
            self.label
        );
        Ok(())
    }
}

/// Fails unmatched deliveries so they show red in GitHub for redelivery.
struct Reject;

impl EventHandler for Reject {
    type Error = AppError;

    async fn handle(&self, meta: EventMeta, _event: WebhookEvent) -> Result<(), Self::Error> {
        Err(AppError::Unhandled {
            kind: meta.kind,
            action: meta.action,
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;
    let verifier = Verifier::new(Secret::new(secret));

    let dispatcher = Dispatcher::<AppError>::builder()
        .always(Auditor)
        .on(
            (
                EventKind::PullRequest,
                [Action::Opened, Action::Synchronize, Action::Reopened],
            ),
            |meta: EventMeta, event: WebhookEvent| async move {
                println!(
                    "triage {:?} on {}",
                    meta.action,
                    event
                        .repository
                        .map_or_else(String::new, |repository| repository.name)
                );
                Ok::<_, Infallible>(())
            },
        )
        .handle_with(Labeler {
            label: "needs-review".into(),
        })
        .fallback(Reject)
        .build();

    let webhook = WebhookReceiverBuilder::new(verifier).build(Persist {
        store: Arc::new(Store::default()),
        dispatcher,
    });

    let app: Router = Router::new().route("/webhook", post_service(webhook));
    let address = std::env::var("WEBHOOK_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("listening on http://{address}/webhook");
    axum::serve(listener, app).await?;

    Ok(())
}
