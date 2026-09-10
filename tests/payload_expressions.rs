//! Consumer coverage for kind expressions valid in an associated constant.
//!
//! Kept independent of the other test helpers and dev-dependencies so
//! this file can also run as an isolated consumer's test target, without
//! unrelated Syn feature unification supplying expression support.

#![cfg(feature = "derive")]

use octoevents::{EventKind, Payload};

const CUSTOM_KIND: EventKind = EventKind::Push;

#[derive(serde::Deserialize, Payload)]
#[payload(octoevents::EventKind::Issues)]
struct Path;

#[derive(serde::Deserialize, Payload)]
#[payload(CUSTOM_KIND)]
struct UserConstant;

#[derive(serde::Deserialize, Payload)]
#[payload(EventKind::from_static("future_event"))]
struct ConstCall;

#[derive(serde::Deserialize, Payload)]
#[payload(if ISSUES { EventKind::Issues } else { EventKind::PullRequest })]
struct Conditional<const ISSUES: bool>;

#[derive(serde::Deserialize, Payload)]
#[payload(const { EventKind::from_static("issues") })]
struct InlineConst;

#[test]
fn associated_constant_expressions_set_the_payload_kind() {
    assert_eq!(Path::KIND, EventKind::Issues);
    assert_eq!(UserConstant::KIND, EventKind::Push);
    assert_eq!(ConstCall::KIND.as_str(), "future_event");
    assert_eq!(Conditional::<true>::KIND, EventKind::Issues);
    assert_eq!(Conditional::<false>::KIND, EventKind::PullRequest);
    assert_eq!(InlineConst::KIND, EventKind::Issues);
}
