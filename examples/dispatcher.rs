//! A GitHub App receiver: a webhook handler that persists, deduplicates and
//! dead-letters, wrapping a dispatcher that only routes.
//!
//! Run with real deliveries forwarded by `gh webhook forward` (see the README):
//!
//! ```console
//! GITHUB_WEBHOOK_SECRET=development-secret \
//!   cargo run --example dispatcher --features tower,octocrab
//! ```
//!
//! [`Inbox`] is the `WebhookHandler` the receiver is built from. It stores
//! every verified envelope, bytes included, before anything is routed;
//! answers a redelivery of a delivery ID it already holds with success
//! without routing it, so a redelivery from GitHub never runs the handlers
//! twice; and reads the outcome `dispatch` reports to dead-letter the
//! envelope of a kind the dispatcher never registered, bytes still in hand,
//! without turning it into an error. None of that fits a dispatcher tier,
//! which can continue or fail but never skip: the wrapper is where that
//! policy lives, and the dispatcher only routes. Once stored, the envelope
//! is the store's to replay: a handler failure after that point is
//! recovered from the store, not by asking GitHub to redeliver.
//!
//! Inside the dispatcher, both handler flavours appear, each a struct or
//! closure with its own error type:
//!
//! - [`Auditor`] is a `WebhookHandler` in the `always` tier: it runs for
//!   every delivery, reads the metadata off the envelope, and, with nothing
//!   decoded on its behalf, runs even for a payload octocrab cannot represent.
//! - [`Labeler`] is a `PayloadHandler` over octocrab's pull-request payload;
//!   its kind comes from that type, so registering it names only the action
//!   it wants, and other actions never reach it or decode for it.
//! - The triage closure is a `PayloadHandler` over octocrab's decoded
//!   `WebhookEvent`, registered with `on` for some pull-request actions; the
//!   input type is what needs the `octocrab` feature, not the registration.
//!
//! The receiver is built with an `on_error` observer that logs every failed
//! delivery, source chain included, since the receiver answers a handler
//! error with a bare 500 and says nothing else: the dispatcher's
//! `DispatchError` names the tier, the delivery, and the line that registered
//! the failing handler.

// The handlers here print instead of awaiting a database or the GitHub API,
// which is what a real `async fn handle` would do.
#![allow(clippy::unused_async_trait_impl)]

use std::{convert::Infallible, error::Error as _, sync::Mutex};

use axum::{Router, routing::post_service};
use octocrab::models::webhook_events::{WebhookEvent, payload::PullRequestWebhookEventPayload};
use octoevents::{
    Action, DecodeError, DispatchError, Dispatcher, Envelope, EventKind, EventMeta, Match,
    PayloadHandler, Secret, Verifier, WebhookHandler, WebhookReceiverBuilder,
};

/// The application error every handler inside the dispatcher converts into.
///
/// One `From<DecodeError>` covers every decode the dispatcher performs.
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

impl From<Infallible> for AppError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}

/// A stand-in for a database: remembers which deliveries were stored, and
/// keeps the envelopes an operator has to look at.
#[derive(Default)]
struct Store {
    delivery_ids: Mutex<Vec<String>>,
    dead_letters: Mutex<Vec<Envelope>>,
}

/// What a real store reports when it cannot be reached; here, a poisoned lock.
#[derive(Debug, thiserror::Error)]
#[error("the delivery store is unavailable")]
struct StoreError;

impl Store {
    /// Remembers the delivery, or reports `false` when it was already stored.
    fn insert(&self, delivery_id: &str) -> Result<bool, StoreError> {
        let mut ids = self.delivery_ids.lock().map_err(|_| StoreError)?;
        if ids.iter().any(|id| id == delivery_id) {
            return Ok(false);
        }
        ids.push(delivery_id.to_owned());
        Ok(true)
    }

    /// Sets an envelope aside for an operator, bytes included.
    fn dead_letter(&self, envelope: Envelope) -> Result<(), StoreError> {
        self.dead_letters
            .lock()
            .map_err(|_| StoreError)?
            .push(envelope);
        Ok(())
    }
}

/// Everything the wrapper can fail with: its own store, or whatever the
/// dispatcher reports, tier and registration site included.
#[derive(Debug, thiserror::Error)]
enum InboxError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Dispatch(#[from] DispatchError<AppError>),
}

/// Persists, deduplicates and dead-letters around a dispatcher that only
/// routes.
///
/// The persist-before-route advice from the crate docs, with the two
/// decisions a tier cannot make: a redelivery of a stored delivery ID is
/// answered with success and not routed, and the envelope of a kind the route
/// table does not know is dead-lettered for an operator to look at, bytes
/// included, and stays green in GitHub, since redelivery would change
/// nothing. An action GitHub added to a kind this app handles is tolerated.
/// Errors from the handlers that ran pass through either way.
struct Inbox {
    store: Store,
    dispatcher: Dispatcher<AppError>,
}

impl WebhookHandler for Inbox {
    type Error = InboxError;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        // Stored first, so a delivery whose envelope could not be stored is
        // never routed, and a redelivery is recognised before any handler
        // runs for it a second time.
        if !self.store.insert(&envelope.meta.delivery_id)? {
            println!(
                "skip {}: already stored, not routed",
                envelope.meta.delivery_id
            );
            return Ok(());
        }
        println!(
            "stored {} ({} bytes)",
            envelope.meta.delivery_id,
            envelope.raw.len()
        );

        // The dispatcher takes the envelope by value; the clone shares the
        // bytes, so the wrapper still holds them afterwards.
        let outcome = self.dispatcher.dispatch(envelope.clone()).await;
        match outcome.matched {
            Match::Matched | Match::UnmatchedAction => Ok(outcome.result?),
            Match::UnmatchedKind => {
                outcome.result?;
                println!(
                    "dead-letter {} {} ({} bytes)",
                    envelope.meta.delivery_id,
                    envelope.meta.kind,
                    envelope.raw.len()
                );
                Ok(self.store.dead_letter(envelope)?)
            }
        }
    }
}

/// Runs for every delivery, reading only what `EventMeta` carries off the
/// envelope. Its error type says it cannot fail.
struct Auditor;

impl WebhookHandler for Auditor {
    type Error = Infallible;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        let meta = &envelope.meta;
        println!(
            "audit {} {} {:?} from {}",
            meta.delivery_id,
            meta.kind,
            meta.action,
            meta.sender.as_deref().unwrap_or("unknown"),
        );
        Ok(())
    }
}

/// Labels a pull request. Receives the decoded payload and no raw bytes; which
/// actions reach it is decided where it is registered, not here.
struct Labeler {
    label: String, // stands in for a GitHub API client
}

impl PayloadHandler<PullRequestWebhookEventPayload> for Labeler {
    type Error = Infallible;

    async fn handle(
        &self,
        meta: EventMeta,
        payload: PullRequestWebhookEventPayload,
    ) -> Result<(), Self::Error> {
        // A GitHub App acts on the API as the installation that delivered the
        // event, which `EventMeta` carries. A repository webhook (what
        // `gh webhook forward` creates) has none, so there is nothing to act as.
        let Some(installation) = meta.installation_id else {
            println!(
                "skip labeling PR #{}: not delivered through a GitHub App installation",
                payload.number
            );
            return Ok(());
        };
        println!(
            "label PR #{} '{}' as {} via installation {installation}",
            payload.number,
            payload.pull_request.title.unwrap_or_default(),
            self.label
        );
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;
    let verifier = Verifier::new(Secret::new(secret));

    // Routing only: what runs for which kind and action. Whether a delivery
    // is routed at all is the wrapper's decision.
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
        .on_payload_action(
            [Action::Opened],
            Labeler {
                label: "needs-review".into(),
            },
        )
        .build();

    // The receiver answers a handler error with a bare 500 (the response is
    // GitHub's delivery record, not a log), so the observer is where an
    // operator learns why a delivery failed. A dispatch error names the tier,
    // the delivery, and the line that registered the failing handler; its
    // source is the application error.
    let webhook = WebhookReceiverBuilder::new(verifier)
        .on_error(|_: &EventMeta, error: &InboxError| {
            eprintln!("{error}");
            let mut cause = error.source();
            while let Some(error) = cause {
                eprintln!("  caused by: {error}");
                cause = error.source();
            }
        })
        .build(Inbox {
            store: Store::default(),
            dispatcher,
        });

    let app: Router = Router::new().route("/webhook", post_service(webhook));
    let address = std::env::var("WEBHOOK_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("listening on http://{address}/webhook");
    axum::serve(listener, app).await?;

    Ok(())
}
