//! The policy seam: a handler over the envelope that persists, deduplicates
//! and dead-letters, wrapping a dispatcher that only routes.
//!
//! The last rung after `quickstart`, `axum` and `dispatcher`. Run it with real
//! deliveries forwarded by `gh webhook forward` (see the README), forwarding
//! `pull_request` events:
//!
//! ```console
//! GITHUB_WEBHOOK_SECRET=development-secret \
//!   cargo run --example policy_seam --features tower,octocrab
//! ```
//!
//! `WEBHOOK_ADDRESS` overrides the address it listens on, `127.0.0.1:3000` by
//! default. A repository webhook, which is what `gh webhook forward` creates,
//! carries no installation ID, so [`Labeler`] prints its skip line for every
//! delivery; a GitHub App's deliveries carry one.
//!
//! A dispatcher tier can continue or fail but never skip, so three decisions
//! do not fit in one: persist the envelope before anything is routed; answer
//! a redelivery of a stored delivery ID with success without routing it; and
//! set an unmatched delivery aside for an operator without failing it. They
//! live in the *policy seam*, the `Handler<Envelope>` the receiver is built
//! from that calls `dispatch` itself and reads the [`Outcome`] it reports:
//! [`Inbox`] here. The dispatcher only routes.
//!
//! [`Inbox`] stores every envelope the receiver hands it, bytes included,
//! before anything is routed: every verified delivery but the `ping` GitHub
//! sends on creating the webhook, which the receiver answers itself under the
//! default `handle_ping(false)` this example keeps. A redelivery from GitHub
//! carries the delivery ID of the first
//! attempt, so a delivery ID already in the store is answered with success
//! and not routed, and the handlers never run twice for it. That holds after
//! a handler failure too: the first attempt was stored before it was routed,
//! so it is the store's to recover, by an operator or a job re-dispatching
//! the stored envelope, not GitHub's to redeliver. And the envelope of a kind
//! the route table never registered is dead-lettered, bytes still in hand, and
//! stays green in GitHub, since a redelivery would change nothing; an action
//! GitHub added to a kind this app handles is tolerated. Errors from the
//! handlers that ran pass through either way.
//!
//! Inside the dispatcher, handlers over three inputs appear, as structs and
//! as a closure, each with the error type it has; the dispatcher boxes every
//! one at its registration, so no enum joins them:
//!
//! - [`Auditor`] is a `Handler<Envelope>` in the `always` tier: it runs for
//!   every delivery the dispatcher is handed, reads the metadata off the
//!   envelope, and, with nothing decoded on its behalf, runs even for a
//!   payload octocrab cannot represent. It cannot fail, and says so with
//!   `Infallible`. It does not see what the seam answers before calling
//!   `dispatch`, a redelivery, nor the `ping` the receiver answered itself; a
//!   count of every delivery the receiver hands over belongs at the top of
//!   [`Inbox`], and one that must include the `ping` needs the receiver
//!   built with `handle_ping(true)` as well.
//! - [`Labeler`] is a `Handler<Event<PullRequestWebhookEventPayload>>`: the
//!   meta beside octocrab's pull-request payload. Its kind comes from that
//!   type, so its matcher names only the action it wants (a *relative*
//!   matcher), and other actions never reach it or decode for it. Its error
//!   is `BoxError`, so `?` converts whatever the GitHub API client would
//!   return.
//! - The triage closure is a handler over `Event<WebhookEvent>`, octocrab's
//!   decoded event with the meta. `WebhookEvent` decodes any kind and so
//!   declares none, so its matcher spells the kind beside the actions (an
//!   *absolute* matcher). The input type is what needs the `octocrab`
//!   feature, not the registration.
//!
//! The receiver is built with an `on_error` observer that logs every failed
//! delivery, source chain included, since the receiver answers a handler
//! error with a bare 500 and says nothing else: the dispatcher's
//! `DispatchError` names the tier, the delivery, the failing handler and the
//! line that registered it, and its source is the handler's error, which the
//! observer downcasts to tell a decode failure from the application's own.
//!
//! The tests at the bottom drive [`Inbox`] with envelopes from
//! [`Envelope::new`], including a redelivery after a handler failure; run
//! them with `cargo test --example policy_seam --features tower,octocrab`.

// The handlers here print where a real `async fn handle` would await a
// database or the GitHub API. The lint expectation says so; delete it once a
// body has an `.await`.
#![expect(clippy::unused_async_trait_impl)]

use std::{
    collections::HashMap,
    convert::Infallible,
    error::Error as _,
    sync::{Mutex, PoisonError},
};

use axum::{Router, routing::post_service};
use octocrab::models::webhook_events::{WebhookEvent, payload::PullRequestWebhookEventPayload};
use octoevents::{
    Action, BoxError, DecodeError, DispatchError, Dispatcher, Envelope, Event, EventKind,
    EventMeta, Handler, Match, Verifier, WebhookReceiverBuilder, WebhookSecret,
};

/// A stand-in for a database: every envelope stored, by delivery ID, and the
/// delivery IDs an operator has to look at.
///
/// A real store keeps the envelope, not the ID alone: a delivery whose
/// handler failed is answered 500, GitHub redelivers it under the same ID,
/// and [`Inbox`] answers that redelivery with success without routing it. The
/// stored envelope is what makes that safe; with the ID alone the delivery
/// would be lost while GitHub shows green.
#[derive(Default)]
struct Store {
    envelopes: Mutex<HashMap<String, Envelope>>,
    dead_letters: Mutex<Vec<String>>,
}

/// What a real store reports when it cannot be reached; here, a poisoned lock.
#[derive(Debug, thiserror::Error)]
#[error("the delivery store is unavailable")]
struct StoreError;

impl<T> From<PoisonError<T>> for StoreError {
    fn from(_: PoisonError<T>) -> Self {
        Self
    }
}

impl Store {
    /// Stores the envelope under its delivery ID, or, when that ID is already
    /// stored, leaves the first envelope in place and returns it: the first
    /// attempt, bytes included, which is what a recovery job re-dispatches
    /// after a handler failure.
    fn insert(&self, envelope: &Envelope) -> Result<Option<Envelope>, StoreError> {
        let mut envelopes = self.envelopes.lock()?;
        if let Some(stored) = envelopes.get(&envelope.meta.delivery_id) {
            return Ok(Some(stored.clone()));
        }
        envelopes.insert(envelope.meta.delivery_id.clone(), envelope.clone());
        Ok(None)
    }

    /// Marks a stored delivery for an operator to look at.
    fn dead_letter(&self, delivery_id: &str) -> Result<(), StoreError> {
        self.dead_letters.lock()?.push(delivery_id.to_owned());
        Ok(())
    }
}

/// Everything the seam can fail with: its own store, or whatever the
/// dispatcher reports, tier, handler and registration site included.
#[derive(Debug, thiserror::Error)]
enum InboxError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
}

/// The policy seam: persists, deduplicates and dead-letters around a
/// dispatcher that only routes.
///
/// The persist-before-route advice from the crate docs, with the two
/// decisions a tier cannot make: a redelivery of a stored delivery ID is
/// answered with success and not routed, and the envelope of a kind the route
/// table does not know is dead-lettered for an operator to look at, bytes
/// included, and stays green in GitHub, since a redelivery would change
/// nothing. An action GitHub added to a kind this app handles is tolerated.
/// Errors from the handlers that ran pass through either way.
struct Inbox {
    store: Store,
    dispatcher: Dispatcher,
}

impl Handler<Envelope> for Inbox {
    type Error = InboxError;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        // Stored first, so a delivery whose envelope could not be stored is
        // never routed, and a redelivery is recognized before any handler
        // runs for it a second time.
        if let Some(stored) = self.store.insert(&envelope)? {
            println!(
                "skip {}: already stored ({} bytes), not routed",
                stored.meta.delivery_id,
                stored.raw_payload.len()
            );
            return Ok(());
        }
        println!(
            "stored {} ({} bytes)",
            envelope.meta.delivery_id,
            envelope.raw_payload.len()
        );

        // The dispatcher takes the envelope by value; the clone shares the
        // bytes, so the seam still holds them afterwards.
        let outcome = self.dispatcher.dispatch(envelope.clone()).await;
        match outcome.matched {
            Match::Matched | Match::UnmatchedAction => Ok(outcome.result?),
            Match::UnmatchedKind => {
                outcome.result?;
                println!(
                    "dead-letter {} {} ({} bytes)",
                    envelope.meta.delivery_id,
                    envelope.meta.kind,
                    envelope.raw_payload.len()
                );
                Ok(self.store.dead_letter(&envelope.meta.delivery_id)?)
            }
        }
    }
}

/// Runs for every delivery the dispatcher is handed, reading only what
/// `EventMeta` carries off the envelope. Printing cannot fail, and the error
/// type says so.
struct Auditor;

impl Handler<Envelope> for Auditor {
    type Error = Infallible;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        let meta = &envelope.meta;
        println!(
            "audit {} {} {:?} from {}",
            meta.delivery_id,
            meta.kind,
            meta.action,
            meta.sender
                .as_ref()
                .map_or("unknown", |sender| sender.login.as_str()),
        );
        Ok(())
    }
}

/// Labels a pull request. Receives the meta and the decoded payload and no
/// payload bytes; which actions reach it is decided where it is registered, not
/// here.
struct Labeler {
    label: String, // stands in for a GitHub API client
}

impl Handler<Event<PullRequestWebhookEventPayload>> for Labeler {
    type Error = BoxError;

    async fn handle(
        &self,
        Event { meta, payload }: Event<PullRequestWebhookEventPayload>,
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

/// Routing only: what runs for which kind and action. Whether a delivery is
/// routed at all is the seam's decision.
fn dispatcher() -> Dispatcher {
    Dispatcher::builder()
        .always(Auditor)
        // Absolute matcher: `WebhookEvent` decodes any kind, so the kind is
        // spelled here beside the actions.
        .on(
            (
                EventKind::PullRequest,
                [Action::Opened, Action::Synchronize, Action::Reopened],
            ),
            |Event { meta, payload }: Event<WebhookEvent>| async move {
                println!(
                    "triage {:?} on {}",
                    meta.action,
                    payload
                        .repository
                        .map_or_else(String::new, |repository| repository.name)
                );
                Ok::<_, BoxError>(())
            },
        )
        // Relative matcher: the kind, `pull_request`, comes from the payload
        // type `Labeler` handles, so only the action is said.
        .on(
            [Action::Opened],
            Labeler {
                label: "needs-review".into(),
            },
        )
        .build()
}

/// Prints where a delivery failed and why, before the receiver answers 500.
///
/// The receiver's response is GitHub's delivery record, not a log, so the
/// observer is where an operator without a `tracing` subscriber learns why a
/// delivery failed. A dispatch error names the tier, the delivery, the
/// failing handler and the line that registered it; its source is the
/// handler's error, boxed, which a downcast gets back.
fn report(_: &EventMeta, error: &InboxError) {
    eprintln!("{error}");
    let mut cause = error.source();
    while let Some(error) = cause {
        eprintln!("  caused by: {error}");
        cause = error.source();
    }
    if let InboxError::Dispatch(dispatch) = error
        && dispatch.source.is::<DecodeError>()
    {
        eprintln!("  (octocrab's model no longer fits GitHub's payload: a deploy, not a page)");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;
    let verifier = Verifier::new(WebhookSecret::new(secret));

    let webhook = WebhookReceiverBuilder::new(verifier)
        .on_error(report)
        .build(Inbox {
            store: Store::default(),
            dispatcher: dispatcher(),
        });

    let app: Router = Router::new().route("/webhook", post_service(webhook));
    let address = std::env::var("WEBHOOK_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("listening on http://{address}/webhook");
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use octoevents::header;

    use super::*;

    /// A `pull_request.opened` payload octocrab's model accepts: the fixture
    /// the crate's own tests use.
    const OPENED: &str = include_str!("../tests/fixtures/pull_request.opened.json");

    /// Counts the deliveries that reach the dispatcher, from its `always`
    /// tier, so a test can tell a routed delivery from one the seam answered.
    #[derive(Clone, Default)]
    struct Counter(Arc<AtomicUsize>);

    impl Counter {
        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl Handler<Envelope> for Counter {
        type Error = Infallible;

        async fn handle(&self, _: Envelope) -> Result<(), Self::Error> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The example's inbox, its dispatcher counting what reaches it.
    fn inbox() -> (Inbox, Counter) {
        let counter = Counter::default();
        let inbox = Inbox {
            store: Store::default(),
            dispatcher: Dispatcher::builder()
                .always(counter.clone())
                .on(
                    [Action::Opened],
                    Labeler {
                        label: "needs-review".into(),
                    },
                )
                .build(),
        };
        (inbox, counter)
    }

    /// A delivery is stored before it is routed, and routed once.
    #[tokio::test]
    async fn a_delivery_is_stored_then_routed() {
        let (inbox, counter) = inbox();
        let envelope = Envelope::new("delivery-1", EventKind::PullRequest, OPENED);

        inbox.handle(envelope.clone()).await.unwrap();

        assert_eq!(counter.count(), 1);
        assert_eq!(
            inbox.store.envelopes.lock().unwrap().get("delivery-1"),
            Some(&envelope)
        );
    }

    /// A redelivery carries the first attempt's delivery ID: it is answered
    /// with success and the handlers do not run again.
    #[tokio::test]
    async fn a_redelivery_is_answered_with_success_without_routing() {
        let (inbox, counter) = inbox();
        let envelope = Envelope::new("delivery-1", EventKind::PullRequest, OPENED);

        inbox.handle(envelope.clone()).await.unwrap();
        inbox.handle(envelope).await.unwrap();

        assert_eq!(counter.count(), 1);
    }

    /// The case the store exists for: a handler fails, GitHub redelivers, and
    /// the redelivery is answered with success without routing. The delivery
    /// is not lost, because the envelope was stored, bytes included, before
    /// the handler ran; recovery re-dispatches it from the store.
    #[tokio::test]
    async fn a_redelivery_after_a_handler_failure_is_recoverable_from_the_store() {
        #[derive(Debug, thiserror::Error)]
        #[error("the GitHub API is unavailable")]
        struct Api;

        let counter = Counter::default();
        let inbox = Inbox {
            store: Store::default(),
            dispatcher: Dispatcher::builder()
                .always(counter.clone())
                .on(EventKind::PullRequest, |_: Envelope| async {
                    Err::<(), _>(Api)
                })
                .build(),
        };
        let envelope = Envelope::new("delivery-1", EventKind::PullRequest, OPENED);

        // The first attempt fails at the handler: GitHub is answered 500.
        let error = inbox.handle(envelope.clone()).await.unwrap_err();
        let InboxError::Dispatch(dispatch) = &error else {
            panic!("{error}");
        };
        assert!(dispatch.source.is::<Api>(), "{error}");

        // The redelivery is answered with success and not routed.
        inbox.handle(envelope.clone()).await.unwrap();
        assert_eq!(counter.count(), 1);

        // The store still holds the first attempt, bytes included: the
        // delivery is the store's to recover.
        let stored = inbox.store.envelopes.lock().unwrap()["delivery-1"].clone();
        assert_eq!(stored.raw_payload, envelope.raw_payload);
        let recovered = inbox.dispatcher.dispatch(stored).await;
        assert_eq!(recovered.matched, Match::Matched);
    }

    /// A kind the route table never registered is dead-lettered, not failed,
    /// and GitHub sees success.
    #[tokio::test]
    async fn an_unknown_kind_is_dead_lettered_and_succeeds() {
        let (inbox, counter) = inbox();
        let envelope = Envelope::new(
            "delivery-1",
            EventKind::Push,
            br#"{"ref":"refs/heads/main"}"#,
        );

        inbox.handle(envelope).await.unwrap();

        assert_eq!(counter.count(), 1, "the always tier ran");
        assert_eq!(
            *inbox.store.dead_letters.lock().unwrap(),
            vec!["delivery-1".to_owned()]
        );
    }

    /// An action GitHub added to a kind this app handles is tolerated: routed
    /// nowhere, dead-lettered nowhere, answered with success.
    #[tokio::test]
    async fn an_added_action_of_a_known_kind_is_tolerated() {
        let (inbox, _) = inbox();
        let envelope = Envelope::new(
            "delivery-1",
            EventKind::PullRequest,
            br#"{"action":"future_action","number":2}"#,
        );

        inbox.handle(envelope).await.unwrap();

        assert!(inbox.store.dead_letters.lock().unwrap().is_empty());
    }

    /// The receiver end to end, with the example's own dispatcher: a signed
    /// `pull_request.opened` is answered 204, and its redelivery too.
    #[tokio::test]
    async fn the_receiver_answers_a_signed_delivery_and_its_redelivery() {
        let verifier = Verifier::new(WebhookSecret::new("test-secret"));
        let webhook = WebhookReceiverBuilder::new(verifier.clone())
            .on_error(report)
            .build(Inbox {
                store: Store::default(),
                dispatcher: dispatcher(),
            });
        let request = || {
            http::Request::builder()
                .method("POST")
                .uri("/webhook")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::DELIVERY_ID, "delivery-1")
                .header(header::EVENT_NAME, "pull_request")
                .header(header::SIGNATURE, verifier.sign(OPENED.as_bytes()))
                .body(OPENED.to_string())
                .unwrap()
        };

        assert_eq!(webhook.receive(request()).await.status(), 204);
        assert_eq!(webhook.receive(request()).await.status(), 204);
    }
}
