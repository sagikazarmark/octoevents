//! A dispatcher with every tier and a handler over every input, behind a
//! receiver with its knobs set.
//!
//! The rung after `axum`, which has no dispatcher, and before `policy_seam`,
//! which wraps one. Run it with real deliveries forwarded by `gh webhook
//! forward` (the README's "Try it" section), forwarding `issues` events:
//!
//! ```console
//! GITHUB_WEBHOOK_SECRET=development-secret cargo run --example dispatcher --features tower
//! ```
//!
//! `WEBHOOK_ADDRESS` overrides the address it listens on, `127.0.0.1:3000` by
//! default; `GITHUB_WEBHOOK_PREVIOUS_SECRET`, when set, is a second secret the
//! verifier also accepts, the rotation window [`Verifier::also`] opens.
//!
//! The dispatcher runs three tiers per delivery, in order, and the first
//! error ends the dispatch:
//!
//! - **always**: [`audit`], a handler over the [`Envelope`], runs first for
//!   every delivery the dispatcher is handed, bytes included and nothing
//!   decoded on its behalf.
//! - **route**: the handlers registered with `on` whose kind and action match.
//!   [`label`] takes the payload alone, decoded as the [`IssueView`] view; its
//!   kind comes from the view's type, so its matcher says only the action (a
//!   *relative* matcher). [`notify`] takes the meta beside the same view, as
//!   [`Event<IssueView>`](Event), under two actions. [`revoke`] takes the
//!   [`EventMeta`] alone, which declares no kind, so its matcher spells the
//!   kind and the action (an *absolute* matcher); nothing is decoded for it.
//! - **fallback**: [`log_unrouted`], another handler over the envelope, runs
//!   only when no route matched: a kind the route table never registered
//!   (`push`), or an action GitHub added to one it did (`issues.closed`
//!   here). It leaves the delivery green in GitHub; a strict one would fail
//!   it instead.
//!
//! Each handler keeps its own error type, and the dispatcher boxes it where
//! the handler is registered. A payload the view does not fit fails the
//! delivery at the handler that needed the decode, and the `DispatchError`
//! names that handler and the line that registered it; the `on_error`
//! observer prints it, then walks the chain of sources for the why.
//!
//! The tests at the bottom drive the dispatcher with envelopes from
//! [`Envelope::new`] and read the [`Match`](octoevents::Match) each reports;
//! run them with `cargo test --example dispatcher --features tower`.

use std::error::Error as _;

use axum::{Router, routing::post_service};
use octoevents::{
    Action, BoxError, DecodeError, DispatchError, Dispatcher, Envelope, Event, EventKind,
    EventMeta, Payload, Verifier, WebhookReceiver, WebhookReceiverBuilder, WebhookSecret,
};

/// A view over an `issues` payload: the fields the handlers read, and the
/// kind they decode from. Every other field GitHub sends is ignored, so a
/// field GitHub adds elsewhere in the document changes nothing.
#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::Issues)]
struct IssueView {
    issue: Issue,
}

#[derive(serde::Deserialize)]
struct Issue {
    number: u64,
    title: String,
}

/// Always tier: the envelope, bytes included, for every delivery.
async fn audit(envelope: Envelope) -> Result<(), BoxError> {
    let meta = &envelope.meta;
    println!(
        "audit {} {} {:?} ({} bytes)",
        meta.delivery_id,
        meta.kind,
        meta.action,
        envelope.raw_payload.len()
    );
    Ok(())
}

/// Route tier, the payload alone: decoded as the view, kind checked. Which
/// actions reach it is decided where it is registered, not here.
async fn label(issue: IssueView) -> Result<(), BoxError> {
    println!("label #{} '{}'", issue.issue.number, issue.issue.title);
    Ok(())
}

/// Route tier, the meta beside the payload, as one input destructured in the
/// parameter.
async fn notify(Event { meta, payload }: Event<IssueView>) -> Result<(), BoxError> {
    println!(
        "{}: #{} {:?}",
        meta.delivery_id,
        payload.issue.number,
        meta.action.as_ref().map_or("?", Action::as_str)
    );
    Ok(())
}

/// Route tier, the meta alone: `installation.deleted` needs the installation
/// ID and no view of the payload, so nothing is decoded.
async fn revoke(meta: EventMeta) -> Result<(), BoxError> {
    println!("revoke tokens for installation {:?}", meta.installation_id);
    Ok(())
}

/// Fallback tier: whatever no route matched. It cannot see why.
async fn log_unrouted(envelope: Envelope) -> Result<(), BoxError> {
    let meta = &envelope.meta;
    println!(
        "unrouted {} {} {:?}",
        meta.delivery_id, meta.kind, meta.action
    );
    Ok(())
}

/// The dispatcher: what runs for which kind and action.
fn dispatcher() -> Dispatcher {
    Dispatcher::builder()
        .always(audit)
        .on(Action::Opened, label) // `issues`, from `IssueView`
        .on([Action::Opened, Action::Reopened], notify) // `issues`, from `Event<IssueView>`
        .on((EventKind::Installation, Action::Deleted), revoke) // `EventMeta` declares no kind
        .fallback(log_unrouted)
        .build()
}

/// The receiver around the dispatcher, with its knobs set: a body limit under
/// GitHub's 25 MiB cap, the `ping` passed through to the `always` tier, and
/// an observer that prints where a delivery failed and why.
fn receiver(verifier: Verifier) -> WebhookReceiver<Dispatcher> {
    WebhookReceiverBuilder::new(verifier)
        // `issues` payloads are small; GitHub never sends more than 25 MiB.
        .body_limit(1024 * 1024)
        // The `ping` GitHub sends on creating the webhook reaches `audit` too,
        // instead of being answered 204 before any handler runs.
        .handle_ping(true)
        // The response is a bare 500; this is where the error reaches code.
        .on_error(|_: &EventMeta, error: &DispatchError| {
            eprintln!("{error}");
            let mut cause = error.source();
            while let Some(error) = cause {
                eprintln!("  caused by: {error}");
                cause = error.source();
            }
            if error.source.is::<DecodeError>() {
                eprintln!("  (a view no longer fits GitHub's payload: a deploy, not a page)");
            }
        })
        .build(dispatcher())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let mut verifier = Verifier::new(WebhookSecret::new(std::env::var("GITHUB_WEBHOOK_SECRET")?));
    // Rotation: the new secret under `new`, the old one under `also` until
    // deliveries signed with it have drained.
    if let Ok(previous) = std::env::var("GITHUB_WEBHOOK_PREVIOUS_SECRET") {
        verifier = verifier.also(previous.parse()?);
    }

    let app: Router = Router::new().route("/webhook", post_service(receiver(verifier)));
    let address = std::env::var("WEBHOOK_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("listening on http://{address}/webhook");
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use octoevents::{Match, Tier, header};

    use super::*;

    const OPENED: &str =
        r#"{"action":"opened","issue":{"number":7,"title":"Hello"},"installation":{"id":42}}"#;

    /// Every tier that applies runs and the route table reports the match.
    #[tokio::test]
    async fn an_opened_issue_matches_and_every_handler_succeeds() {
        let envelope = Envelope::new("delivery-1", EventKind::Issues, OPENED);

        let outcome = dispatcher().dispatch(envelope).await;

        assert_eq!(outcome.matched, Match::Matched);
        outcome.result.unwrap();
    }

    /// The kind is registered, the action is not: the fallback runs, and the
    /// delivery succeeds.
    #[tokio::test]
    async fn a_closed_issue_is_unmatched_by_action_and_still_succeeds() {
        let envelope = Envelope::new(
            "delivery-1",
            EventKind::Issues,
            br#"{"action":"closed","issue":{"number":7,"title":"Hello"}}"#,
        );

        let outcome = dispatcher().dispatch(envelope).await;

        assert_eq!(outcome.matched, Match::UnmatchedAction);
        outcome.result.unwrap();
    }

    /// A kind the route table never registered.
    #[tokio::test]
    async fn a_push_is_unmatched_by_kind_and_still_succeeds() {
        let envelope = Envelope::new(
            "delivery-1",
            EventKind::Push,
            br#"{"ref":"refs/heads/main"}"#,
        );

        let outcome = dispatcher().dispatch(envelope).await;

        assert_eq!(outcome.matched, Match::UnmatchedKind);
        outcome.result.unwrap();
    }

    /// A handler over the meta alone decodes nothing, so a payload with
    /// nothing but the action and the installation still reaches it.
    #[tokio::test]
    async fn an_installation_deleted_reaches_the_meta_handler() {
        let envelope = Envelope::new(
            "delivery-1",
            EventKind::Installation,
            br#"{"action":"deleted","installation":{"id":42}}"#,
        );

        let outcome = dispatcher().dispatch(envelope).await;

        assert_eq!(outcome.matched, Match::Matched);
        outcome.result.unwrap();
    }

    /// A payload the view does not fit fails the delivery at the first
    /// handler that needed the decode, in the route tier, with the decode
    /// error as the source; `audit` ran before it.
    #[tokio::test]
    async fn a_payload_the_view_does_not_fit_fails_at_the_decode() {
        let envelope = Envelope::new(
            "delivery-1",
            EventKind::Issues,
            br#"{"action":"opened","issue":{"number":7}}"#,
        );

        let outcome = dispatcher().dispatch(envelope).await;

        assert_eq!(outcome.matched, Match::Matched);
        let error = outcome.result.unwrap_err();
        assert_eq!(error.tier, Tier::Route);
        assert!(error.source.is::<DecodeError>(), "{error}");
        assert_eq!(error.delivery_id, "delivery-1");
    }

    /// The receiver end to end: a request signed by the verifier it is built
    /// with is answered 204, one signed by another verifier 401.
    #[tokio::test]
    async fn the_receiver_answers_a_signed_delivery() {
        let verifier = Verifier::new(WebhookSecret::new("test-secret"));
        let webhook = receiver(verifier.clone());

        let signed = |signer: &Verifier| {
            http::Request::builder()
                .method("POST")
                .uri("/webhook")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::DELIVERY_ID, "delivery-1")
                .header(header::EVENT_NAME, "issues")
                .header(header::SIGNATURE, signer.sign(OPENED.as_bytes()))
                .body(OPENED.to_string())
                .unwrap()
        };

        let accepted = webhook.receive(signed(&verifier)).await;
        let refused = webhook
            .receive(signed(&Verifier::new(WebhookSecret::new("other-secret"))))
            .await;

        assert_eq!(accepted.status(), 204);
        assert_eq!(refused.status(), 401);
    }

    /// With `handle_ping(true)`, the `ping` GitHub sends on creating the
    /// webhook reaches the dispatcher (its `always` tier, then the fallback,
    /// since no route registers `ping`) and is still answered 204.
    #[tokio::test]
    async fn a_ping_reaches_the_dispatcher_and_is_answered_204() {
        let verifier = Verifier::new(WebhookSecret::new("test-secret"));
        let webhook = receiver(verifier.clone());
        let body = r#"{"zen":"Design for failure.","hook_id":1}"#;

        let request = http::Request::builder()
            .method("POST")
            .uri("/webhook")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::DELIVERY_ID, "delivery-1")
            .header(header::EVENT_NAME, "ping")
            .header(header::SIGNATURE, verifier.sign(body.as_bytes()))
            .body(body.to_string())
            .unwrap();

        let response = webhook.receive(request).await;

        assert_eq!(response.status(), 204);
    }
}
