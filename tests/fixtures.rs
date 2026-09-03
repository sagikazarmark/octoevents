//! Signed webhook fixture corpus sourced from captured GitHub payload examples.

#![cfg(feature = "octocrab")]

use bytes::Bytes;
use hmac::{Hmac, KeyInit, Mac};
use octoevents::{Envelope, HeaderView, Secret, Verifier};
use sha2::Sha256;

struct Fixture {
    body: &'static [u8],
    event: &'static str,
    action: Option<&'static str>,
}

#[test]
fn signed_real_fixture_corpus_is_routable_and_typed() {
    let fixtures = [
        Fixture {
            body: include_bytes!("fixtures/pull_request.opened.json"),
            event: "pull_request",
            action: Some("opened"),
        },
        Fixture {
            body: include_bytes!("fixtures/check_run.completed.json"),
            event: "check_run",
            action: Some("completed"),
        },
        Fixture {
            body: include_bytes!("fixtures/installation.created.json"),
            event: "installation",
            action: Some("created"),
        },
        Fixture {
            body: include_bytes!("fixtures/installation_repositories.removed.json"),
            event: "installation_repositories",
            action: Some("removed"),
        },
        Fixture {
            body: include_bytes!("fixtures/ping.json"),
            event: "ping",
            action: None,
        },
        Fixture {
            body: include_bytes!("fixtures/unknown.json"),
            event: "future_event",
            action: None,
        },
    ];

    for fixture in fixtures {
        let envelope = signed_envelope(fixture.body, fixture.event);

        assert_eq!(envelope.meta.kind.as_str(), fixture.event);
        assert_eq!(
            envelope
                .meta
                .action
                .as_ref()
                .map(octoevents::Action::as_str),
            fixture.action
        );
        assert!(
            envelope.parse_typed().is_ok(),
            "event: {} should be represented by octocrab",
            fixture.event,
        );
    }
}

#[test]
fn signed_unrepresentable_json_keeps_the_raw_body() {
    let body = include_bytes!("fixtures/unrepresentable.json");
    let envelope = signed_envelope(body, "pull_request");

    assert!(envelope.parse_typed().is_err());
    assert_eq!(envelope.raw, bytes::Bytes::from_static(body));
}

fn signed_envelope(body: &'static [u8], event: &str) -> Envelope {
    let signature = signature(b"fixture-secret", body);
    let headers = HeaderView::new()
        .signature(&signature)
        .delivery_id("fixture-delivery")
        .event_name(event)
        .content_type("application/json");

    Envelope::from_signed(
        &Verifier::new(Secret::new("fixture-secret")),
        &headers,
        Bytes::from_static(body),
    )
    .unwrap()
}

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
