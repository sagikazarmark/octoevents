//! The dispatcher's tests, beside the production code so they share the
//! crate's fixtures and see the module's private items. Grouped by concern:
//! the tiers and the order they run in, matching and the outcome, dispatch
//! errors, the inputs a handler can be over, the matchers a registration
//! takes, and the routes over octocrab's types.

use std::{future::Future, pin::Pin, sync::Arc};

use tokio::sync::Mutex;

use crate::{DispatchError, EventKind, Match, Outcome, Payload, test_support::AppError};

type Calls = Arc<Mutex<Vec<&'static str>>>;
type Recorded<E> = Pin<Box<dyn Future<Output = Result<(), E>> + Send>>;

/// A consumer view that accepts any `pull_request` payload, so a payload
/// route for that kind can be registered with or without octocrab.
#[derive(serde::Deserialize)]
struct AnyPullRequest {}
impl Payload for AnyPullRequest {
    const KIND: EventKind = EventKind::PullRequest;
}

/// A `pull_request` view the fixtures cannot satisfy, for tests that need
/// a decode to fail at a known route.
#[derive(serde::Deserialize)]
struct Number {
    #[expect(
        dead_code,
        reason = "the field is required so the decode fails; nothing reads it"
    )]
    number: u64,
}
impl Payload for Number {
    const KIND: EventKind = EventKind::PullRequest;
}

/// The handlers' result with the dispatch error unwrapped to its source,
/// read back as the test's [`AppError`], for tests that check which handler
/// failed rather than where it was registered.
fn unwrapped(outcome: Outcome) -> Result<(), AppError> {
    outcome.result.map_err(app_error)
}

/// [`unwrapped`] keeping the match beside the result.
fn unwrapped_outcome(outcome: Outcome) -> (Match, Result<(), AppError>) {
    (outcome.matched, outcome.result.map_err(app_error))
}

/// The [`AppError`] behind a dispatch error's boxed source.
fn app_error(error: DispatchError) -> AppError {
    AppError::from_boxed(error.into_source())
}

/// A handler over any input `I` that appends `value` to the shared log and
/// then answers `result`: `Ok(())` lets the chain continue, an `Err` fails
/// the delivery there with `AppError::Handler` of the name. A test fails a
/// handler with its own `value`, so the log entry and the error name the
/// same handler.
///
/// The input is named at the registration, `recording::<EventMeta>(..)`,
/// since the matcher alone does not fix it, and it says what is decoded on
/// the handler's behalf: nothing for `EventMeta`, a clone for `Envelope`,
/// the payload for a `Payload` view.
fn recording<I: 'static>(
    calls: &Calls,
    value: &'static str,
    result: Result<(), &'static str>,
) -> impl Fn(I) -> Recorded<AppError> + Send + Sync + 'static {
    let calls = Arc::clone(calls);
    move |_| {
        let calls = Arc::clone(&calls);
        Box::pin(async move {
            calls.lock().await.push(value);
            result.map_err(AppError::Handler)
        })
    }
}

/// The three tiers: the order they run in, what the always and fallback
/// tiers receive, when the fallback runs, and that each chain fails fast.
mod tiers {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::{AnyPullRequest, Calls, Recorded, recording, unwrapped};
    use crate::{
        Action, AnyAction, Dispatcher, Envelope, EventKind, EventMeta, Payload,
        test_support::{
            AppError, check_run_completed, installation_created, ping, pull_request_opened,
            unknown, unrepresentable,
        },
    };

    #[tokio::test]
    async fn tiers_run_always_then_action_then_kind_in_registration_order() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                EventKind::PullRequest,
                recording::<EventMeta>(&calls, "kind-1", Ok(())),
            )
            .always(recording::<Envelope>(&calls, "always-1", Ok(())))
            .on(
                (EventKind::PullRequest, Action::Opened),
                recording::<EventMeta>(&calls, "action-1", Ok(())),
            )
            .on(
                EventKind::PullRequest,
                recording::<EventMeta>(&calls, "kind-2", Ok(())),
            )
            .always(recording::<Envelope>(&calls, "always-2", Ok(())))
            .on(
                (EventKind::PullRequest, Action::Opened),
                recording::<EventMeta>(&calls, "action-2", Ok(())),
            )
            .fallback(recording::<Envelope>(&calls, "fallback", Ok(())))
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();

        assert_eq!(
            calls.lock().await.as_slice(),
            [
                "always-1", "always-2", "action-1", "action-2", "kind-1", "kind-2"
            ]
        );
    }

    #[tokio::test]
    async fn payload_routes_run_the_action_chain_before_the_kind_chain_in_registration_order() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "kind-1", Ok(())),
            )
            .on(
                [Action::Opened],
                recording::<AnyPullRequest>(&calls, "action-1", Ok(())),
            )
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "kind-2", Ok(())),
            )
            .on(
                [Action::Opened],
                recording::<AnyPullRequest>(&calls, "action-2", Ok(())),
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();

        assert_eq!(
            calls.lock().await.as_slice(),
            ["action-1", "action-2", "kind-1", "kind-2"]
        );
    }

    #[tokio::test]
    async fn tiers_run_always_then_routes_then_fallback_in_registration_order() {
        // Registration order is interleaved across tiers on purpose: the tier
        // decides when a handler runs, and only order within a tier follows
        // registration.
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "route-1", Ok(())),
            )
            .always(recording::<Envelope>(&calls, "always-1", Ok(())))
            .fallback(recording::<Envelope>(&calls, "fallback-1", Ok(())))
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "route-2", Ok(())),
            )
            .always(recording::<Envelope>(&calls, "always-2", Ok(())))
            .fallback(recording::<Envelope>(&calls, "fallback-2", Ok(())))
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(
            calls.lock().await.as_slice(),
            ["always-1", "always-2", "route-1", "route-2"]
        );

        calls.lock().await.clear();
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
        assert_eq!(
            calls.lock().await.as_slice(),
            ["always-1", "always-2", "fallback-1", "fallback-2"]
        );
    }

    #[tokio::test]
    async fn always_runs_for_every_delivery_without_counting_as_a_match() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .always(recording::<Envelope>(&calls, "audit", Ok(())))
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "pull-request", Ok(())),
            )
            .fallback(recording::<Envelope>(&calls, "unmatched", Err("unmatched")))
            .build();

        // Only `always` applies: the strict fallback still rejects it.
        assert_eq!(
            unwrapped(dispatcher.dispatch(installation_created()).await),
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["audit", "unmatched"]);
    }

    #[tokio::test]
    async fn always_runs_for_a_payload_octocrab_cannot_represent() {
        let calls = Calls::default();
        let handler_calls = Arc::clone(&calls);
        let dispatcher = Dispatcher::builder()
            .always(move |envelope: Envelope| {
                let calls = Arc::clone(&handler_calls);
                async move {
                    calls.lock().await.push("audit");
                    assert_eq!(envelope.meta.kind, EventKind::PullRequest);
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .build();

        // Nothing is decoded on the always tier's behalf, so the delivery
        // succeeds although octocrab cannot represent it.
        dispatcher.dispatch(unrepresentable()).await.result.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["audit"]);
    }

    #[tokio::test]
    async fn always_and_fallback_receive_the_envelope_with_its_bytes() {
        type Seen = Arc<Mutex<Vec<(&'static str, EventKind, bytes::Bytes)>>>;

        fn forward(
            seen: &Seen,
            tier: &'static str,
        ) -> impl Fn(Envelope) -> Recorded<AppError> + Send + Sync + 'static {
            let seen = Arc::clone(seen);
            move |envelope| {
                let seen = Arc::clone(&seen);
                Box::pin(async move {
                    seen.lock()
                        .await
                        .push((tier, envelope.meta.kind, envelope.raw_payload));
                    Ok(())
                })
            }
        }

        // A forwarder in either tier sees the exact bytes the envelope
        // carries, not a metadata-only view of it.
        let seen = Seen::default();
        let dispatcher = Dispatcher::builder()
            .always(forward(&seen, "always"))
            .fallback(forward(&seen, "fallback"))
            .build();

        let envelope = check_run_completed();
        dispatcher.dispatch(envelope.clone()).await.result.unwrap();

        assert_eq!(
            seen.lock().await.as_slice(),
            [
                ("always", EventKind::CheckRun, envelope.raw_payload.clone()),
                ("fallback", EventKind::CheckRun, envelope.raw_payload),
            ]
        );
    }

    #[tokio::test]
    async fn a_strict_fallback_reports_its_own_error_for_an_unmatched_unrepresentable_payload() {
        #[derive(serde::Deserialize)]
        struct AnyCheckRun {}
        impl Payload for AnyCheckRun {
            const KIND: EventKind = EventKind::CheckRun;
        }

        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(AnyAction, |_: AnyCheckRun| async {
                Ok::<_, std::convert::Infallible>(())
            })
            .fallback(recording::<Envelope>(&calls, "unmatched", Err("unmatched")))
            .build();

        // Nothing routes `pull_request`, so the fallback decides: it never
        // decodes, so the answer is "unhandled", not a decode error.
        assert_eq!(
            unwrapped(dispatcher.dispatch(unrepresentable()).await),
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["unmatched"]);
    }

    #[tokio::test]
    async fn the_fallback_chain_runs_in_order_only_when_nothing_matched() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "pull-request", Ok(())),
            )
            .fallback(recording::<Envelope>(&calls, "log", Ok(())))
            .fallback(recording::<Envelope>(&calls, "reject", Err("reject")))
            .build();

        assert_eq!(
            unwrapped(dispatcher.dispatch(check_run_completed()).await),
            Err(AppError::Handler("reject"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["log", "reject"]);

        calls.lock().await.clear();
        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["pull-request"]);
    }

    #[tokio::test]
    async fn unmatched_deliveries_succeed_when_the_fallback_chain_is_empty() {
        let dispatcher = Dispatcher::builder()
            .on(AnyAction, |_: AnyPullRequest| async {
                Ok::<_, AppError>(())
            })
            .build();

        dispatcher.dispatch(unknown()).await.result.unwrap();
        dispatcher.dispatch(ping()).await.result.unwrap();
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
    }

    #[tokio::test]
    async fn the_always_and_fallback_chains_fail_fast() {
        let calls = Calls::default();
        let always = Dispatcher::builder()
            .always(recording::<Envelope>(&calls, "always", Err("always")))
            .always(recording::<Envelope>(&calls, "always-after", Ok(())))
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "routed", Ok(())),
            )
            .build();
        assert_eq!(
            unwrapped(always.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("always"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["always"]);

        calls.lock().await.clear();
        let fallback = Dispatcher::builder()
            .fallback(recording::<Envelope>(&calls, "fallback", Err("fallback")))
            .fallback(recording::<Envelope>(&calls, "fallback-after", Ok(())))
            .build();
        assert_eq!(
            unwrapped(fallback.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("fallback"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["fallback"]);
    }

    #[tokio::test]
    async fn the_routed_chains_fail_fast() {
        let calls = Calls::default();
        let routed = Dispatcher::builder()
            .on(
                (EventKind::PullRequest, Action::Opened),
                recording::<EventMeta>(&calls, "action", Err("action")),
            )
            .on(
                (EventKind::PullRequest, Action::Opened),
                recording::<EventMeta>(&calls, "action-after", Ok(())),
            )
            .on(
                EventKind::PullRequest,
                recording::<EventMeta>(&calls, "kind", Ok(())),
            )
            .build();
        assert_eq!(
            unwrapped(routed.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("action"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["action"]);

        calls.lock().await.clear();
        let kind_wide = Dispatcher::builder()
            .on(
                EventKind::PullRequest,
                recording::<EventMeta>(&calls, "kind", Err("kind")),
            )
            .on(
                EventKind::PullRequest,
                recording::<EventMeta>(&calls, "kind-after", Ok(())),
            )
            .build();
        assert_eq!(
            unwrapped(kind_wide.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("kind"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["kind"]);
    }
}

/// Matching and the outcome: the route table decides the match whichever
/// tier fails, an unmatched outcome says whether the kind was known, and a
/// wrapper over `dispatch` sets the policy the tiers cannot.
mod matching {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::{AnyPullRequest, Calls, Number, recording, unwrapped, unwrapped_outcome};
    use crate::{
        Action, AnyAction, DispatchError, Dispatcher, Envelope, EventKind, EventMeta, Handler,
        Match,
        test_support::{
            AppError, check_run_completed, envelope, pull_request, pull_request_opened, unknown,
            unrepresentable,
        },
    };

    #[tokio::test]
    async fn a_matched_failure_is_distinguishable_from_an_unmatched_fallback_failure() {
        let dispatcher = Dispatcher::builder()
            .on(AnyAction, |_: AnyPullRequest| async {
                Err::<(), _>(AppError::Handler("routed"))
            })
            .fallback(|_: Envelope| async { Err::<(), _>(AppError::Handler("unmatched")) })
            .build();

        // Both deliveries fail. The result alone cannot say whether a routed
        // handler or a strict fallback failed them; the match can.
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(pull_request_opened()).await),
            (Match::Matched, Err(AppError::Handler("routed")))
        );
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(check_run_completed()).await),
            (Match::UnmatchedKind, Err(AppError::Handler("unmatched")))
        );
    }

    #[tokio::test]
    async fn an_unmatched_outcome_says_whether_the_route_table_knows_the_kind() {
        let dispatcher = Dispatcher::builder()
            .on([Action::Opened], |_: AnyPullRequest| async {
                Ok::<_, AppError>(())
            })
            .build();

        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(pull_request_opened()).await),
            (Match::Matched, Ok(()))
        );

        // Routes exist for `pull_request`, none for `closed`: the kind is
        // known, so a strict policy can tolerate an action it did not
        // register. The same holds for a delivery of the kind carrying no
        // action at all.
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(pull_request(Action::Closed)).await),
            (Match::UnmatchedAction, Ok(()))
        );
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(unrepresentable()).await),
            (Match::UnmatchedAction, Ok(()))
        );

        // No route mentions `check_run`, nor a kind this crate does not know.
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(check_run_completed()).await),
            (Match::UnmatchedKind, Ok(()))
        );
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(unknown()).await),
            (Match::UnmatchedKind, Ok(()))
        );
    }

    #[test]
    fn a_match_displays_as_the_label_a_policy_logs() {
        // The span vocabulary, snake_case, so a policy that prints the match
        // beside the dispatch span's `outcome` reads as one set of labels.
        assert_eq!(Match::Matched.to_string(), "matched");
        assert_eq!(Match::UnmatchedAction.to_string(), "unmatched_action");
        assert_eq!(Match::UnmatchedKind.to_string(), "unmatched_kind");
        assert_eq!(Match::UnmatchedKind.as_str(), "unmatched_kind");
    }

    #[tokio::test]
    async fn the_match_is_decided_by_the_route_table_whichever_tier_fails() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .always(recording::<Envelope>(&calls, "audit", Err("audit")))
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "routed", Ok(())),
            )
            .build();

        // The always tier fails before routing begins. The route table still
        // says which delivery would have been routed and which would not.
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(pull_request_opened()).await),
            (Match::Matched, Err(AppError::Handler("audit")))
        );
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(check_run_completed()).await),
            (Match::UnmatchedKind, Err(AppError::Handler("audit")))
        );
        assert_eq!(calls.lock().await.as_slice(), ["audit", "audit"]);
    }

    #[tokio::test]
    async fn handle_keeps_the_result_and_drops_the_match() {
        let dispatcher = Dispatcher::builder()
            .on(AnyAction, |_: AnyPullRequest| async {
                Err::<(), _>(AppError::Handler("routed"))
            })
            .build();

        // What the receiver sees: an unmatched delivery succeeds, a matched
        // one reports its handler's error, and neither says which it was.
        dispatcher.handle(check_run_completed()).await.unwrap();
        assert_eq!(
            dispatcher
                .handle(pull_request_opened())
                .await
                .map_err(super::app_error),
            Err(AppError::Handler("routed"))
        );
    }

    #[tokio::test]
    async fn a_wrapper_forwards_unmatched_deliveries_with_their_bytes() {
        /// Sets the policy the tiers cannot: an action GitHub added to a kind
        /// the dispatcher handles is tolerated, and a delivery of a kind it
        /// never registered is dead-lettered, bytes included, instead of
        /// being turned into an error.
        struct DeadLetter {
            dispatcher: Dispatcher,
            letters: Arc<Mutex<Vec<Envelope>>>,
        }

        impl Handler<Envelope> for DeadLetter {
            // The dispatcher's error passes through, tier, handler and registration site included.
            type Error = DispatchError;

            async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
                // The dispatcher takes the envelope by value; the clone shares
                // the bytes, so the wrapper still holds them afterwards.
                let outcome = self.dispatcher.dispatch(envelope.clone()).await;
                match outcome.matched {
                    Match::Matched | Match::UnmatchedAction => outcome.result,
                    Match::UnmatchedKind => {
                        outcome.result?;
                        self.letters.lock().await.push(envelope);
                        Ok(())
                    }
                }
            }
        }

        let calls = Calls::default();
        let letters = Arc::new(Mutex::new(Vec::new()));
        let wrapper = DeadLetter {
            dispatcher: Dispatcher::builder()
                .always(recording::<Envelope>(&calls, "audit", Ok(())))
                .on(
                    [Action::Opened],
                    recording::<AnyPullRequest>(&calls, "triage", Ok(())),
                )
                .build(),
            letters: Arc::clone(&letters),
        };

        // Matched, and known kind with an unregistered action: nothing is
        // dead-lettered and both succeed.
        wrapper.handle(pull_request_opened()).await.unwrap();
        wrapper.handle(pull_request(Action::Closed)).await.unwrap();
        assert!(letters.lock().await.is_empty());

        // Unknown kind: dead-lettered as the exact envelope the dispatcher
        // saw, and still a success towards GitHub.
        let envelope = check_run_completed();
        wrapper.handle(envelope.clone()).await.unwrap();
        assert_eq!(letters.lock().await.as_slice(), [envelope]);
        assert_eq!(
            calls.lock().await.as_slice(),
            ["audit", "triage", "audit", "audit"]
        );
    }

    #[tokio::test]
    async fn a_registered_kind_with_no_matching_action_still_falls_back() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                (EventKind::PullRequest, Action::Closed),
                recording::<EventMeta>(&calls, "closed", Ok(())),
            )
            .fallback(recording::<Envelope>(&calls, "unmatched", Err("unmatched")))
            .build();

        assert_eq!(
            unwrapped(dispatcher.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["unmatched"]);
    }

    #[tokio::test]
    async fn a_handler_registered_for_some_actions_matches_only_those() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                [Action::Opened, Action::Reopened],
                recording::<AnyPullRequest>(&calls, "triage", Ok(())),
            )
            .fallback(recording::<Envelope>(&calls, "unmatched", Err("unmatched")))
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["triage"]);

        calls.lock().await.clear();
        dispatcher
            .dispatch(pull_request(Action::Reopened))
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["triage"]);

        // `closed` was never registered, so the kind alone earns no match:
        // the strict fallback rejects it instead of the handler silently
        // widening to every pull-request action.
        calls.lock().await.clear();
        assert_eq!(
            unwrapped(dispatcher.dispatch(pull_request(Action::Closed)).await),
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["unmatched"]);
    }

    #[tokio::test]
    async fn an_unregistered_action_is_not_decoded() {
        // Had the route decoded `Number`, the delivery would have failed with
        // a decode error.
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on([Action::Opened], |_: Number| async {
                Ok::<_, std::convert::Infallible>(())
            })
            .fallback(recording::<Envelope>(&calls, "unmatched", Err("unmatched")))
            .build();

        // An action this crate does not know yet: nothing registered cares,
        // so nothing is decoded, and the fallback answers.
        let future = envelope(EventKind::PullRequest, br#"{"action":"future_action"}"#);
        assert_eq!(
            unwrapped(dispatcher.dispatch(future).await),
            Err(AppError::Handler("unmatched"))
        );
        assert_eq!(calls.lock().await.as_slice(), ["unmatched"]);
    }
}

/// Dispatch errors: the tier, delivery, handler name and registration site
/// a failure names, where a decode failure is reported, what `Display` and
/// `source` say, and the bounds the dispatcher places on its error type.
mod errors {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::{AnyPullRequest, Calls, Number, Recorded, recording};
    use crate::{
        Action, AnyAction, BoxError, DecodeError, DispatchError, Dispatcher, Envelope, Event,
        EventKind, EventMeta, FromEnvelope, Handler, Match, Tier,
        test_support::{AppError, envelope, installation_created, ping, pull_request_opened},
    };

    #[tokio::test]
    async fn a_failure_names_the_tier_the_delivery_and_the_registration_site() {
        // The location is that of the registration method's name, so the
        // failing handler is registered on the line after `line!()`.
        let calls = Calls::default();
        let builder = Dispatcher::builder();
        let registration_site = line!() + 1;
        let builder = builder.on(
            AnyAction,
            recording::<AnyPullRequest>(&calls, "routed", Err("routed")),
        );
        let dispatcher = builder.build();

        let error = dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap_err();

        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.delivery_id, "delivery");
        assert_eq!(error.kind, EventKind::PullRequest);
        assert_eq!(error.action, Some(Action::Opened));
        assert_eq!(error.registration_site.file(), file!());
        assert_eq!(error.registration_site.line(), registration_site);
        assert_eq!(
            AppError::from_boxed(error.source),
            AppError::Handler("routed")
        );
    }

    #[tokio::test]
    async fn every_registration_method_names_its_tier_and_its_own_line() {
        let calls = Calls::default();

        /// Registers a failing handler with the named method and pairs the
        /// builder with the line of the call, so each case pins its own
        /// registration site however the call is formatted.
        macro_rules! registered {
            ($method:ident($($argument:expr),*)) => {
                (Dispatcher::builder().$method($($argument),*), line!())
            };
        }

        // One failing handler per registration method, so the locations are
        // distinct and each error must carry its own. Registration order is
        // irrelevant to the tier: the method decides it.
        let cases = [
            (
                registered!(always(recording::<Envelope>(
                    &calls,
                    "always",
                    Err("always")
                ))),
                Tier::Always,
                "always",
            ),
            (
                registered!(on(
                    EventKind::PullRequest,
                    recording::<EventMeta>(&calls, "on", Err("on"))
                )),
                Tier::Route,
                "on",
            ),
            (
                registered!(on(
                    [Action::Opened],
                    recording::<AnyPullRequest>(&calls, "action", Err("action"))
                )),
                Tier::Route,
                "action",
            ),
            (
                registered!(on(
                    AnyAction,
                    recording::<AnyPullRequest>(&calls, "kind", Err("kind"))
                )),
                Tier::Route,
                "kind",
            ),
            (
                registered!(fallback(recording::<Envelope>(
                    &calls,
                    "fallback",
                    Err("fallback")
                ))),
                Tier::Fallback,
                "fallback",
            ),
        ];
        for ((builder, line), tier, value) in cases {
            let error = builder
                .build()
                .dispatch(pull_request_opened())
                .await
                .result
                .unwrap_err();
            assert_eq!(error.tier, tier, "{value}");
            assert_eq!(error.registration_site.line(), line, "{value}");
            assert_eq!(AppError::from_boxed(error.source), AppError::Handler(value));
        }
    }

    #[tokio::test]
    async fn the_fallback_tier_reports_a_delivery_without_an_action() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .fallback(recording::<Envelope>(&calls, "unmatched", Err("unmatched")))
            .build();

        let error = dispatcher.dispatch(ping()).await.result.unwrap_err();

        assert_eq!(error.tier, Tier::Fallback);
        assert_eq!(error.kind, EventKind::Ping);
        assert_eq!(error.action, None);
    }

    #[tokio::test]
    async fn a_failure_names_the_handler_by_its_type_name() {
        use std::any::{type_name, type_name_of_val};

        /// An `async fn` item: its type is the function's path.
        async fn revoke(_: EventMeta) -> Result<(), AppError> {
            Err(AppError::Handler("revoke"))
        }

        /// A struct handler: its type is the struct's path.
        struct Revoker {
            calls: Calls,
        }

        impl Handler<EventMeta> for Revoker {
            type Error = AppError;

            async fn handle(&self, _: EventMeta) -> Result<(), Self::Error> {
                self.calls.lock().await.push("revoker");
                Err(AppError::Handler("revoker"))
            }
        }

        // The name is `type_name`'s output for the registered handler, so an
        // operator reading the error finds the function or struct by name
        // without opening the registration site. The equality pins the exact
        // string; `ends_with` pins its shape, a path ending in the item's
        // name, independently of how `type_name` spells the prefix.
        let dispatcher = Dispatcher::builder()
            .on(EventKind::Installation, revoke)
            .build();
        let error = dispatcher
            .dispatch(installation_created())
            .await
            .result
            .unwrap_err();
        assert_eq!(error.handler, type_name_of_val(&revoke));
        assert!(error.handler.ends_with("::revoke"), "{}", error.handler);
        assert_eq!(
            AppError::from_boxed(error.source),
            AppError::Handler("revoke")
        );

        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                EventKind::Installation,
                Revoker {
                    calls: Arc::clone(&calls),
                },
            )
            .build();
        let error = dispatcher
            .dispatch(installation_created())
            .await
            .result
            .unwrap_err();
        assert_eq!(error.handler, type_name::<Revoker>());
        assert!(error.handler.ends_with("::Revoker"), "{}", error.handler);
        assert_eq!(
            AppError::from_boxed(error.source),
            AppError::Handler("revoker")
        );
        assert_eq!(calls.lock().await.as_slice(), ["revoker"]);
    }

    #[tokio::test]
    async fn a_decode_failure_is_reported_at_the_handler_that_needed_the_decode() {
        fn needs_number(_: Number) -> Recorded<AppError> {
            Box::pin(async { Ok(()) })
        }

        // The handlers before it never decode, so the failure is attributed to
        // the registration of the handler that needed the decode, not the
        // first one.
        let calls = Calls::default();
        let builder = Dispatcher::builder().always(recording::<Envelope>(&calls, "always", Ok(())));
        let registration_site = line!() + 1;
        let dispatcher = builder.on(AnyAction, needs_number).build();

        let error = dispatcher
            .dispatch(envelope(EventKind::PullRequest, br#"{"action":"opened"}"#))
            .await
            .result
            .unwrap_err();

        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.registration_site.line(), registration_site);
        assert_eq!(AppError::from_boxed(error.source), AppError::Decode);
        assert_eq!(calls.lock().await.as_slice(), ["always"]);
    }

    #[tokio::test]
    async fn a_decode_failure_inside_event_is_reported_at_the_handler_that_needed_it() {
        // The unsatisfiable view wrapped in `Event`: the failure is the
        // wrapper's handler's, at its registration, with the handlers before
        // it untouched.
        fn needs_number(_: Event<Number>) -> Recorded<AppError> {
            Box::pin(async { Ok(()) })
        }

        let calls = Calls::default();
        let builder = Dispatcher::builder()
            .always(recording::<Envelope>(&calls, "always", Ok(())))
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "payload-before", Ok(())),
            );
        let registration_site = line!() + 1;
        let builder = builder.on(AnyAction, needs_number);
        let dispatcher = builder
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "payload-after", Ok(())),
            )
            .build();

        let error = dispatcher
            .dispatch(envelope(EventKind::PullRequest, br#"{"action":"opened"}"#))
            .await
            .result
            .unwrap_err();

        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.registration_site.line(), registration_site);
        assert_eq!(AppError::from_boxed(error.source), AppError::Decode);
        assert_eq!(calls.lock().await.as_slice(), ["always", "payload-before"]);
    }

    #[tokio::test]
    async fn a_consumer_input_failing_for_a_reason_of_its_own_is_reported_at_its_registration() {
        use std::error::Error as _;

        /// The installation ID, required rather than optional. Read off the
        /// meta, so a delivery without one fails for the input's own reason:
        /// neither a kind mismatch nor a JSON error.
        struct InstallationId(u64);
        impl FromEnvelope for InstallationId {
            fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
                envelope
                    .meta
                    .installation_id
                    .map(Self)
                    .ok_or_else(|| DecodeError::input("payload has no installation"))
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let builder = Dispatcher::builder();
        let registration_site = line!() + 1;
        let dispatcher = builder.on(
            (EventKind::Installation, Action::Deleted),
            move |InstallationId(id): InstallationId| {
                let seen = Arc::clone(&handler_seen);
                async move {
                    seen.lock().await.push(id);
                    Ok::<_, std::convert::Infallible>(())
                }
            },
        );
        let dispatcher = dispatcher.build();

        // A delivery carrying the installation reaches the handler as the ID,
        // read from the payload.
        let installed = envelope(
            EventKind::Installation,
            br#"{"action":"deleted","installation":{"id":42}}"#,
        );
        dispatcher.dispatch(installed).await.result.unwrap();
        assert_eq!(seen.lock().await.as_slice(), [42]);

        // One without fails in the route tier at the handler's registration,
        // and the dispatch error's source is the decode error carrying the
        // consumer's own message, not a decode of the payload.
        let error = dispatcher
            .dispatch(envelope(
                EventKind::Installation,
                br#"{"action":"deleted"}"#,
            ))
            .await
            .result
            .unwrap_err();
        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.registration_site.line(), registration_site);
        let reason = error.source().expect("the decode error is the source");
        assert_eq!(reason.to_string(), "payload has no installation");
        assert!(
            matches!(
                reason.downcast_ref::<DecodeError>(),
                Some(DecodeError::Input { source: None, .. })
            ),
            "{reason:?}"
        );
        assert!(reason.source().is_none());
        // The decode failed before the handler ran, so it saw nothing new.
        assert_eq!(seen.lock().await.as_slice(), [42]);
    }

    #[tokio::test]
    async fn display_names_the_tier_the_handler_and_the_registration_site_and_source_yields_the_application_error()
     {
        use std::{any::type_name_of_val, error::Error as _};

        #[derive(Debug, thiserror::Error)]
        enum ServiceError {
            #[error("database is down")]
            Database,
        }

        fn database_down(_: AnyPullRequest) -> Recorded<ServiceError> {
            Box::pin(async { Err(ServiceError::Database) })
        }

        let builder = Dispatcher::builder();
        let registration_site = line!() + 1;
        let dispatcher = builder.on(AnyAction, database_down).build();

        let error = dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap_err();

        // The message names where, not why: the source chain says why, as
        // it does for every other error in the crate.
        assert_eq!(error.registration_site.line(), registration_site);
        assert_eq!(
            error.to_string(),
            format!(
                "delivery delivery (pull_request.opened) failed in the route tier at the handler \
                 `{}` registered at {}",
                type_name_of_val(&database_down),
                error.registration_site
            )
        );
        let source = error.source().expect("the application error is the source");
        assert_eq!(source.to_string(), "database is down");
        assert!(matches!(
            source.downcast_ref::<ServiceError>(),
            Some(ServiceError::Database)
        ));

        // Without an action the parenthesised part is the kind alone.
        let dispatcher = Dispatcher::builder()
            .fallback(|_: Envelope| async { Err::<(), _>(ServiceError::Database) })
            .build();
        let error = dispatcher.dispatch(ping()).await.result.unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("delivery delivery (ping) failed in the fallback tier at the handler"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_handler_returning_the_boxed_error_registers_and_its_error_is_the_source() {
        use std::error::Error as _;

        // The front page's shape: no error enum, `?` converts anything. The
        // dispatch error is an `Error` whose source is the box the handler
        // returned, so a reporter walks one chain whatever the handler's
        // error type was.
        let dispatcher = Dispatcher::builder()
            .on(AnyAction, |_: AnyPullRequest| async {
                Err::<(), BoxError>("boom".into())
            })
            .build();

        let error = dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap_err();

        assert!(error.to_string().contains("route tier"), "{error}");
        assert_eq!(error.source().expect("the boxed error").to_string(), "boom");
        assert_eq!(error.into_source().to_string(), "boom");
    }

    #[tokio::test]
    async fn handlers_with_different_error_types_share_one_dispatcher() {
        // Every handler's error converts into the box at its registration, so
        // the dispatcher has no one error type of its own: an `io::Error` in
        // one tier and a `String` in another register side by side, and each
        // is read back as itself.
        let dispatcher = Dispatcher::builder()
            .always(|_: Envelope| async { Ok::<_, std::io::Error>(()) })
            .on(AnyAction, |_: AnyPullRequest| async {
                Err::<(), _>(std::io::Error::other("routed"))
            })
            .fallback(|_: Envelope| async { Err::<(), _>(String::from("unmatched")) })
            .build();

        let routed = dispatcher.dispatch(pull_request_opened()).await;
        assert_eq!(routed.matched, Match::Matched);
        let source = routed.result.unwrap_err().into_source();
        assert_eq!(
            source
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::Other)
        );

        let unmatched = dispatcher.dispatch(ping()).await;
        assert_eq!(unmatched.matched, Match::UnmatchedKind);
        assert_eq!(
            unmatched.result.unwrap_err().into_source().to_string(),
            "unmatched"
        );
    }

    #[tokio::test]
    async fn a_dispatcher_is_a_route_of_another() {
        // A dispatch error is an `Error`, so a dispatcher registers on another
        // like any handler: it contributes a result and never a match, and
        // the outer error's source is the inner dispatch error, its own
        // source the application error.
        use std::error::Error as _;

        let calls = Calls::default();
        let inner = Dispatcher::builder()
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "inner", Err("inner")),
            )
            .build();
        let outer = Dispatcher::builder()
            .on([EventKind::PullRequest, EventKind::Issues], inner)
            .build();

        let outcome = outer.dispatch(pull_request_opened()).await;
        assert_eq!(outcome.matched, Match::Matched);
        let error = outcome.result.unwrap_err();
        assert_eq!(error.tier, Tier::Route);
        let inner_error = error
            .source()
            .and_then(|source| source.downcast_ref::<DispatchError>())
            .expect("the inner dispatch error is the source");
        assert_eq!(inner_error.tier, Tier::Route);
        assert_eq!(
            inner_error
                .source()
                .and_then(|source| source.downcast_ref::<AppError>()),
            Some(&AppError::Handler("inner"))
        );

        // The outer route table matched `issues`; the inner had no route for
        // it, contributed `Ok`, and the outer outcome says nothing of that.
        calls.lock().await.clear();
        let outcome = outer
            .dispatch(envelope(EventKind::Issues, br#"{"action":"opened"}"#))
            .await;
        assert_eq!(outcome.matched, Match::Matched);
        outcome.result.unwrap();
        assert!(calls.lock().await.is_empty());
    }
}

/// The inputs a handler can be over and what is decoded for each: the meta
/// alone, the envelope with its bytes, a consumer `FromEnvelope` view; the
/// forms a handler takes, a struct behind an `Arc` on any tier or registered
/// twice with a turbofish; and that a payload is decoded from the
/// dispatcher's own envelope.
mod inputs {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::{AnyPullRequest, Calls, recording, unwrapped, unwrapped_outcome};
    use crate::{
        Action, AnyAction, DecodeError, Dispatcher, Envelope, Event, EventKind, EventMeta,
        FromEnvelope, Handler, Match, Payload, Tier,
        test_support::{
            AppError, check_run_completed, envelope, envelope_with_action, installation_created,
            pull_request, pull_request_opened, unrepresentable,
        },
    };

    #[tokio::test]
    async fn on_routes_a_handler_over_the_meta_by_kind_and_action_with_nothing_decoded() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::builder()
            .on(
                (EventKind::Installation, Action::Deleted),
                move |meta: EventMeta| {
                    let seen = Arc::clone(&handler_seen);
                    async move {
                        seen.lock()
                            .await
                            .push((meta.delivery_id, meta.installation_id));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .fallback(|_: Envelope| async { Err::<(), _>(AppError::Handler("unmatched")) })
            .build();

        // Nothing is decoded on the handler's behalf: a payload that is not
        // even a JSON object still reaches it, meta in hand. The payload
        // carries no installation for the constructor to read, so this test
        // assigns it, on purpose.
        let mut envelope = envelope_with_action(
            EventKind::Installation,
            Action::Deleted,
            b"not a json object",
        );
        envelope.meta.installation_id = Some(42);
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(envelope).await),
            (Match::Matched, Ok(()))
        );
        assert_eq!(
            seen.lock().await.as_slice(),
            [("delivery".to_owned(), Some(42))]
        );

        // The route is by kind and action, so another action of the kind is
        // unmatched and the strict fallback answers.
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(installation_created()).await),
            (Match::UnmatchedAction, Err(AppError::Handler("unmatched")))
        );
    }

    #[tokio::test]
    async fn on_routes_a_handler_over_the_envelope_by_kind_with_its_bytes() {
        type Seen = Arc<Mutex<Vec<(EventKind, bytes::Bytes)>>>;

        // A forwarder for one kind: `on(kind, handler over Envelope)` hands
        // over the exact bytes as `always` does, and counts as a match.
        let seen = Seen::default();
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::builder()
            .on(EventKind::CheckRun, move |envelope: Envelope| {
                let seen = Arc::clone(&handler_seen);
                async move {
                    seen.lock()
                        .await
                        .push((envelope.meta.kind, envelope.raw_payload));
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .fallback(|_: Envelope| async { Err::<(), _>(AppError::Handler("unmatched")) })
            .build();

        let envelope = check_run_completed();
        assert_eq!(
            unwrapped_outcome(dispatcher.dispatch(envelope.clone()).await),
            (Match::Matched, Ok(()))
        );
        assert_eq!(
            seen.lock().await.as_slice(),
            [(EventKind::CheckRun, envelope.raw_payload)]
        );

        // Nothing is decoded for it either: a body that is not JSON reaches
        // it whole.
        let not_json = envelope_with_action(EventKind::CheckRun, Action::Completed, b"not json");
        dispatcher.dispatch(not_json).await.result.unwrap();
        assert_eq!(seen.lock().await.len(), 2);

        // Another kind is unmatched as for any routed handler.
        assert_eq!(
            unwrapped(dispatcher.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("unmatched"))
        );
    }

    #[tokio::test]
    async fn on_routes_a_consumer_from_envelope_view_under_several_kinds() {
        /// The sender's login, which every kind's payload carries; not a
        /// `Payload`, since it declares no single kind, so it implements
        /// `FromEnvelope` itself with the kind-free `decode`. Registered once
        /// alone and once inside `Event`, so the handler sees the meta beside
        /// it.
        #[derive(serde::Deserialize)]
        struct Sender {
            sender: Login,
        }
        #[derive(serde::Deserialize)]
        struct Login {
            login: String,
        }
        impl FromEnvelope for Sender {
            fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
                envelope.decode()
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let alone_seen = Arc::clone(&seen);
        let event_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::builder()
            .on(
                [EventKind::PullRequest, EventKind::CheckRun],
                move |sender: Sender| {
                    let seen = Arc::clone(&alone_seen);
                    async move {
                        seen.lock().await.push((None, sender.sender.login));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .on(
                [EventKind::PullRequest, EventKind::CheckRun],
                move |Event { meta, payload }: Event<Sender>| {
                    let seen = Arc::clone(&event_seen);
                    async move {
                        seen.lock()
                            .await
                            .push((Some(meta.kind), payload.sender.login));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .fallback(|_: Envelope| async { Err::<(), _>(AppError::Handler("unmatched")) })
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
        // A kind the view was not registered under is unmatched as usual.
        assert_eq!(
            unwrapped(dispatcher.dispatch(installation_created()).await),
            Err(AppError::Handler("unmatched"))
        );

        assert_eq!(
            seen.lock().await.as_slice(),
            [
                (None, "gagbo".to_owned()),
                (Some(EventKind::PullRequest), "gagbo".to_owned()),
                (None, "Codertocat".to_owned()),
                (Some(EventKind::CheckRun), "Codertocat".to_owned()),
            ]
        );

        // A payload the view does not fit fails at the view's route, as any
        // decode does; the first route over it, alone, is the one reported.
        let error = dispatcher
            .dispatch(envelope(EventKind::PullRequest, br#"{"action":"opened"}"#))
            .await
            .result
            .unwrap_err();
        assert_eq!(error.tier, Tier::Route);
        assert_eq!(AppError::from_boxed(error.source), AppError::Decode);
    }

    #[tokio::test]
    async fn an_arc_shared_handler_over_the_envelope_is_registered_as_itself() {
        /// A handler the test keeps a handle on after registering it, to read
        /// what it saw without a closure adapter.
        struct Auditor {
            seen: Mutex<Vec<EventKind>>,
        }

        impl Handler<Envelope> for Auditor {
            type Error = std::convert::Infallible;

            async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
                self.seen.lock().await.push(envelope.meta.kind);
                Ok(())
            }
        }

        let auditor = Arc::new(Auditor {
            seen: Mutex::new(Vec::new()),
        });
        let dispatcher = Dispatcher::builder()
            .always(Arc::clone(&auditor))
            .fallback(Arc::clone(&auditor))
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();

        // One instance, two registrations, and the test's own handle.
        assert_eq!(
            auditor.seen.lock().await.as_slice(),
            [EventKind::PullRequest, EventKind::PullRequest]
        );
    }

    #[tokio::test]
    async fn an_arc_shared_routed_handler_is_registered_as_itself() {
        struct Labeler {
            seen: Mutex<Vec<(Option<Action>, &'static str)>>,
        }

        impl Handler<Event<AnyPullRequest>> for Labeler {
            type Error = std::convert::Infallible;

            async fn handle(
                &self,
                Event { meta, .. }: Event<AnyPullRequest>,
            ) -> Result<(), Self::Error> {
                self.seen.lock().await.push((meta.action, "payload"));
                Ok(())
            }
        }

        impl Handler<EventMeta> for Labeler {
            type Error = std::convert::Infallible;

            async fn handle(&self, meta: EventMeta) -> Result<(), Self::Error> {
                self.seen.lock().await.push((meta.action, "meta"));
                Ok(())
            }
        }

        let labeler = Arc::new(Labeler {
            seen: Mutex::new(Vec::new()),
        });
        let dispatcher = Dispatcher::builder()
            .on::<Event<AnyPullRequest>, _, _>(AnyAction, Arc::clone(&labeler))
            .on::<EventMeta, _, _>(
                (EventKind::PullRequest, Action::Closed),
                Arc::clone(&labeler),
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        dispatcher
            .dispatch(pull_request(Action::Closed))
            .await
            .result
            .unwrap();

        assert_eq!(
            labeler.seen.lock().await.as_slice(),
            [
                (Some(Action::Opened), "payload"),
                (Some(Action::Closed), "meta"),
                (Some(Action::Closed), "payload"),
            ]
        );
    }

    #[tokio::test]
    async fn a_struct_handling_two_payloads_is_registered_with_a_turbofish() {
        #[derive(serde::Deserialize)]
        struct AnyCheckRun {}
        impl Payload for AnyCheckRun {
            const KIND: EventKind = EventKind::CheckRun;
        }

        struct Labeler {
            calls: Calls,
        }

        impl Handler<AnyPullRequest> for Labeler {
            type Error = AppError;

            async fn handle(&self, _: AnyPullRequest) -> Result<(), AppError> {
                self.calls.lock().await.push("pull-request");
                Ok(())
            }
        }

        impl Handler<AnyCheckRun> for Labeler {
            type Error = AppError;

            async fn handle(&self, _: AnyCheckRun) -> Result<(), AppError> {
                self.calls.lock().await.push("check-run");
                Ok(())
            }
        }

        let calls = Calls::default();
        // The struct alone no longer says which payload is meant, so each
        // registration names it. The dispatcher `Arc`s each registration;
        // the caller shares nothing by hand.
        let dispatcher = Dispatcher::builder()
            .on::<AnyPullRequest, _, _>(
                AnyAction,
                Labeler {
                    calls: Arc::clone(&calls),
                },
            )
            .on::<AnyCheckRun, _, _>(
                AnyAction,
                Labeler {
                    calls: Arc::clone(&calls),
                },
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();

        assert_eq!(calls.lock().await.as_slice(), ["pull-request", "check-run"]);
    }

    #[tokio::test]
    async fn a_routed_handler_over_a_payload_decodes_from_the_dispatchers_envelope_and_not_a_clone_of_it()
     {
        /// Whether the decode saw the only handle on the payload bytes.
        ///
        /// `Envelope::new` copies the payload into bytes with one handle,
        /// and `dispatch` takes the envelope by value, so at decode time the
        /// dispatcher's envelope is the only one unless a route cloned it to
        /// decode from: a clone shares the bytes, is live while
        /// `from_envelope` runs, and `Bytes::is_unique` says so. A `Payload`
        /// with a decode of its own rather than a serde view, so the handler
        /// is routed as a payload's is, under actions alone, and the decode
        /// can look at the envelope it is handed.
        struct SoleHandle(bool);
        impl FromEnvelope for SoleHandle {
            fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
                Ok(Self(envelope.raw_payload.is_unique()))
            }
        }
        impl Payload for SoleHandle {
            const KIND: EventKind = EventKind::PullRequest;
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::builder()
            .on(AnyAction, move |SoleHandle(unique): SoleHandle| {
                let seen = Arc::clone(&handler_seen);
                async move {
                    seen.lock().await.push(unique);
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .build();

        let envelope = pull_request_opened();
        assert!(
            envelope.raw_payload.is_unique(),
            "the test holds one handle"
        );
        dispatcher.dispatch(envelope).await.result.unwrap();

        assert_eq!(seen.lock().await.as_slice(), [true]);
    }

    #[tokio::test]
    async fn a_delivery_with_no_handler_over_webhook_event_never_needs_octocrab() {
        // The same unrepresentable payload succeeds when no handler over
        // `WebhookEvent` needs octocrab's decoding: the always and fallback
        // tiers see the envelope as verified, and a consumer view over the
        // bytes has nothing octocrab must represent.
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .always(recording::<Envelope>(&calls, "always", Ok(())))
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "payload", Ok(())),
            )
            .fallback(recording::<Envelope>(&calls, "unmatched", Err("unmatched")))
            .build();

        dispatcher.dispatch(unrepresentable()).await.result.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["always", "payload"]);
    }
}

/// The matchers a registration takes: every absolute form expands to its
/// routes, and a relative one (actions alone, `AnyAction`) takes the kind
/// from the handler's payload type, `Event<P>` included.
mod matchers {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::{AnyPullRequest, Calls, recording, unwrapped};
    use crate::{
        Action, AnyAction, DecodeError, Dispatcher, Envelope, Event, EventKind, EventMatcher,
        EventMeta, Payload, Tier,
        test_support::{
            AppError, check_run_completed, envelope_with_action, installation_created,
            pull_request_opened,
        },
    };

    #[tokio::test]
    async fn every_matcher_form_expands_to_its_routes() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                [EventKind::PullRequest, EventKind::CheckRun],
                recording::<EventMeta>(&calls, "kinds", Ok(())),
            )
            .on(
                (EventKind::PullRequest, [Action::Opened, Action::Closed]),
                recording::<EventMeta>(&calls, "actions", Ok(())),
            )
            .on(
                [
                    (EventKind::PullRequest, Action::Opened),
                    (EventKind::CheckRun, Action::Completed),
                ],
                recording::<EventMeta>(&calls, "pairs", Ok(())),
            )
            .on(
                EventMatcher::from(EventKind::Installation)
                    .or((EventKind::CheckRun, Action::Completed)),
                recording::<EventMeta>(&calls, "or", Ok(())),
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["actions", "pairs", "kinds"]);

        calls.lock().await.clear();
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["pairs", "or", "kinds"]);

        calls.lock().await.clear();
        dispatcher
            .dispatch(installation_created())
            .await
            .result
            .unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["or"]);

        // The action list is exact: a pull request being synchronized does not
        // reach the [opened, closed] handler.
        calls.lock().await.clear();
        let synchronized = envelope_with_action(
            EventKind::PullRequest,
            Action::Synchronize,
            include_bytes!("../../tests/fixtures/pull_request.opened.json"),
        );
        dispatcher.dispatch(synchronized).await.result.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["kinds"]);
    }

    #[tokio::test]
    async fn on_with_a_payload_under_a_matcher_of_another_kind_fails_at_the_kind() {
        // The bytes of a check run would fit this view over any object; the
        // kind the view declares is what disagrees with the matcher.
        let calls = Calls::default();
        let always_calls = Arc::clone(&calls);
        let handler_calls = Arc::clone(&calls);
        let builder = Dispatcher::builder().always(move |_: Envelope| {
            let calls = Arc::clone(&always_calls);
            async move {
                calls.lock().await.push("always");
                Ok::<_, std::convert::Infallible>(())
            }
        });
        let registration_site = line!() + 1;
        let dispatcher = builder.on(EventKind::CheckRun, move |_: AnyPullRequest| {
            let calls = Arc::clone(&handler_calls);
            async move {
                calls.lock().await.push("routed");
                Ok::<_, std::convert::Infallible>(())
            }
        });

        let error = dispatcher
            .build()
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap_err();

        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.registration_site.line(), registration_site);
        assert!(
            matches!(
                error.source.downcast_ref::<DecodeError>(),
                Some(DecodeError::KindMismatch {
                    expected: EventKind::PullRequest,
                    actual: EventKind::CheckRun,
                })
            ),
            "{:?}",
            error.source
        );
        assert_eq!(calls.lock().await.as_slice(), ["always"]);
    }

    #[tokio::test]
    async fn on_with_a_payload_under_its_spelled_kind_decodes_it_as_any_action_does() {
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                EventKind::PullRequest,
                recording::<AnyPullRequest>(&calls, "on", Ok(())),
            )
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "any-action", Ok(())),
            )
            .on(
                (EventKind::PullRequest, [Action::Opened, Action::Reopened]),
                recording::<AnyPullRequest>(&calls, "on-actions", Ok(())),
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();

        assert_eq!(
            calls.lock().await.as_slice(),
            ["on-actions", "on", "any-action"]
        );
    }

    #[tokio::test]
    async fn any_action_accepts_a_consumer_defined_payload_view() {
        #[derive(serde::Deserialize)]
        struct Conclusion {
            check_run: CheckRunConclusion,
        }

        #[derive(serde::Deserialize)]
        struct CheckRunConclusion {
            conclusion: String,
        }

        impl Payload for Conclusion {
            const KIND: EventKind = EventKind::CheckRun;
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::builder()
            .on(
                AnyAction,
                move |Event { meta, payload }: Event<Conclusion>| {
                    let seen = Arc::clone(&handler_seen);
                    async move {
                        seen.lock()
                            .await
                            .push((meta.delivery_id, payload.check_run.conclusion));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .build();

        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();

        assert_eq!(
            seen.lock().await.as_slice(),
            [("delivery".to_owned(), "success".to_owned())]
        );
    }

    #[tokio::test]
    async fn actions_alone_route_a_handler_over_event_of_a_payload_by_the_payloads_kind() {
        // `Event<P>` is a `Payload` of `P`'s kind, so `AnyAction` and an
        // action list accept a handler over it with no kind said, and the
        // handler receives the meta beside the decoded payload.
        #[derive(serde::Deserialize)]
        struct Conclusion {
            check_run: CheckRunConclusion,
        }
        #[derive(serde::Deserialize)]
        struct CheckRunConclusion {
            conclusion: String,
        }
        impl Payload for Conclusion {
            const KIND: EventKind = EventKind::CheckRun;
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let kind_wide = Arc::clone(&seen);
        let by_action = Arc::clone(&seen);
        let dispatcher = Dispatcher::builder()
            .on(
                AnyAction,
                move |Event { meta, payload }: Event<Conclusion>| {
                    let seen = Arc::clone(&kind_wide);
                    async move {
                        seen.lock().await.push((
                            "kind",
                            meta.delivery_id,
                            meta.action,
                            payload.check_run.conclusion,
                        ));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .on(
                [Action::Completed],
                move |Event { meta, payload }: Event<Conclusion>| {
                    let seen = Arc::clone(&by_action);
                    async move {
                        seen.lock().await.push((
                            "action",
                            meta.delivery_id,
                            meta.action,
                            payload.check_run.conclusion,
                        ));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .fallback(|_: Envelope| async { Err::<(), _>(AppError::Handler("unmatched")) })
            .build();

        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
        assert_eq!(
            seen.lock().await.as_slice(),
            [
                (
                    "action",
                    "delivery".to_owned(),
                    Some(Action::Completed),
                    "success".to_owned()
                ),
                (
                    "kind",
                    "delivery".to_owned(),
                    Some(Action::Completed),
                    "success".to_owned()
                ),
            ]
        );

        // The kind came from `Conclusion` through the wrapper: another kind
        // is unmatched.
        assert_eq!(
            unwrapped(dispatcher.dispatch(pull_request_opened()).await),
            Err(AppError::Handler("unmatched"))
        );
    }
}

/// Routes over octocrab's types: its `WebhookEvent` alone and inside
/// `Event`, its per-kind payload structs under a relative matcher, where a
/// delivery octocrab cannot represent fails, and that each handler's own
/// error converts into the dispatcher's beside theirs.
#[cfg(feature = "octocrab")]
mod octocrab {
    use std::sync::Arc;

    use ::octocrab::models::webhook_events::{
        WebhookEvent, WebhookEventType,
        payload::{PullRequestWebhookEventAction, PullRequestWebhookEventPayload},
    };
    use tokio::sync::Mutex;

    use super::{AnyPullRequest, Calls, recording, unwrapped};
    use crate::{
        Action, AnyAction, Dispatcher, Envelope, Event, EventKind, Handler, Tier,
        test_support::{
            AppError, check_run_completed, envelope, installation_created, pull_request_opened,
            source_as, unrepresentable,
        },
    };

    #[tokio::test]
    async fn on_routes_octocrabs_event_alone_and_inside_event_with_the_meta_beside_it() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let alone_seen = Arc::clone(&seen);
        let event_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::builder()
            .on(EventKind::PullRequest, move |event: WebhookEvent| {
                let seen = Arc::clone(&alone_seen);
                async move {
                    seen.lock().await.push((None, event.kind));
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .on(
                EventKind::PullRequest,
                move |Event { meta, payload }: Event<WebhookEvent>| {
                    let seen = Arc::clone(&event_seen);
                    async move {
                        seen.lock()
                            .await
                            .push((Some(meta.delivery_id), payload.kind));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(
            seen.lock().await.as_slice(),
            [
                (None, WebhookEventType::PullRequest),
                (Some("delivery".to_owned()), WebhookEventType::PullRequest),
            ]
        );
    }

    #[tokio::test]
    async fn any_action_routes_a_handler_by_its_payload_type() {
        struct Labeler {
            seen: Arc<Mutex<Vec<(String, u64, PullRequestWebhookEventAction)>>>,
        }

        impl Handler<Event<PullRequestWebhookEventPayload>> for Labeler {
            type Error = std::convert::Infallible;

            async fn handle(
                &self,
                Event { meta, payload }: Event<PullRequestWebhookEventPayload>,
            ) -> Result<(), Self::Error> {
                self.seen
                    .lock()
                    .await
                    .push((meta.delivery_id, payload.number, payload.action));
                Ok(())
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let dispatcher = Dispatcher::builder()
            .on(
                AnyAction,
                Labeler {
                    seen: Arc::clone(&seen),
                },
            )
            .build();

        dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap();
        assert_eq!(
            seen.lock().await.as_slice(),
            [(
                "delivery".to_owned(),
                2,
                PullRequestWebhookEventAction::Opened
            )]
        );

        // Another kind never reaches it, and with no fallback still succeeds.
        dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap();
        assert_eq!(seen.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn an_action_octocrab_does_not_know_fails_the_delivery_only_at_a_handler_that_needs_it() {
        // octocrab's per-kind action enums have no catch-all, so its payload
        // struct cannot decode an action it does not know. A handler over it
        // registered for the actions it wants is never asked to; a consumer
        // view registered kind-wide still runs.
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .on(
                [Action::Opened],
                |_: PullRequestWebhookEventPayload| async { Ok::<_, std::convert::Infallible>(()) },
            )
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "view", Ok(())),
            )
            .build();

        let future = envelope(
            EventKind::PullRequest,
            br#"{"action":"future_action","number":2}"#,
        );
        dispatcher.dispatch(future.clone()).await.result.unwrap();
        assert_eq!(calls.lock().await.as_slice(), ["view"]);

        // The same handler registered for every action of the kind is asked,
        // and the delivery fails at its decode.
        let kind_wide = Dispatcher::builder()
            .on(AnyAction, |_: PullRequestWebhookEventPayload| async {
                Ok::<_, std::convert::Infallible>(())
            })
            .build();
        assert_eq!(
            unwrapped(kind_wide.dispatch(future).await),
            Err(AppError::Decode)
        );
    }

    #[tokio::test]
    async fn an_event_decode_failure_is_reported_at_the_handler_over_webhook_event_that_needed_it()
    {
        // The payload route before the handler over `WebhookEvent` decodes
        // its own view and succeeds; octocrab's decode fails at that handler,
        // and the error names its registration.
        let calls = Calls::default();
        let builder = Dispatcher::builder().on(
            AnyAction,
            recording::<AnyPullRequest>(&calls, "view", Ok(())),
        );
        let registration_site = line!() + 1;
        let dispatcher = builder.on(
            EventKind::PullRequest,
            recording::<WebhookEvent>(&calls, "event", Ok(())),
        );

        let error = dispatcher
            .build()
            .dispatch(unrepresentable())
            .await
            .result
            .unwrap_err();

        assert_eq!(error.tier, Tier::Route);
        assert_eq!(error.registration_site.line(), registration_site);
        assert_eq!(AppError::from_boxed(error.source), AppError::Decode);
        assert_eq!(calls.lock().await.as_slice(), ["view"]);
    }

    #[tokio::test]
    async fn an_event_decode_failure_stops_the_delivery_at_the_first_handler_over_webhook_event() {
        // The always tier and handlers over a consumer view sit either side
        // of the handlers over `WebhookEvent`, so the log shows exactly
        // where the chain stopped: each `WebhookEvent` route decodes its own
        // input when it runs, and nothing before the first one needed octocrab.
        let calls = Calls::default();
        let dispatcher = Dispatcher::builder()
            .always(recording::<Envelope>(&calls, "always", Ok(())))
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "payload-before", Ok(())),
            )
            .on(
                EventKind::PullRequest,
                recording::<WebhookEvent>(&calls, "event", Ok(())),
            )
            .on(
                AnyAction,
                recording::<AnyPullRequest>(&calls, "payload-after", Ok(())),
            )
            .on(
                EventKind::PullRequest,
                recording::<WebhookEvent>(&calls, "event-after", Ok(())),
            )
            .fallback(recording::<Envelope>(&calls, "unmatched", Err("unmatched")))
            .build();

        assert_eq!(
            unwrapped(dispatcher.dispatch(unrepresentable()).await),
            Err(AppError::Decode)
        );
        assert_eq!(calls.lock().await.as_slice(), ["always", "payload-before"]);
    }

    #[tokio::test]
    async fn each_handlers_error_is_boxed_and_read_back_as_itself() {
        #[derive(Debug, PartialEq, thiserror::Error)]
        #[error("db")]
        struct DbError;
        #[derive(Debug, PartialEq, thiserror::Error)]
        #[error("api")]
        struct ApiError;
        #[derive(Debug, PartialEq, thiserror::Error)]
        #[error("queue")]
        struct QueueError;

        // The property `errors::handlers_with_different_error_types_share_one_dispatcher`
        // pins, over octocrab's inputs: a route over `WebhookEvent` and one
        // over a per-kind payload struct box their errors like any other, and
        // a decode on the handler's behalf changes nothing about the source
        // when the handler itself fails.
        let dispatcher = Dispatcher::builder()
            .always(|envelope: Envelope| async move {
                if envelope.meta.kind == EventKind::Installation {
                    Err(DbError)
                } else {
                    Ok(())
                }
            })
            .on(EventKind::CheckRun, |_: WebhookEvent| async {
                Err::<(), _>(ApiError)
            })
            .on(AnyAction, |_: PullRequestWebhookEventPayload| async {
                Err::<(), _>(QueueError)
            })
            .build();

        let error = dispatcher
            .dispatch(installation_created())
            .await
            .result
            .unwrap_err();
        assert_eq!(source_as::<DbError>(&error), &DbError);
        let error = dispatcher
            .dispatch(check_run_completed())
            .await
            .result
            .unwrap_err();
        assert_eq!(source_as::<ApiError>(&error), &ApiError);
        let error = dispatcher
            .dispatch(pull_request_opened())
            .await
            .result
            .unwrap_err();
        assert_eq!(source_as::<QueueError>(&error), &QueueError);
    }
}
