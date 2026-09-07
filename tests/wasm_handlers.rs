//! Compile-time coverage for non-`Send` handler state on `wasm32`.
//!
//! Cloudflare Workers are single-threaded and hand handlers JavaScript values
//! and `Rc` state. `MaybeSend`/`MaybeSync` relax the handler bounds there, and
//! this file proves handlers over every input compile through the full
//! erasure path with such state. Build it with
//! `cargo build --test wasm_handlers --target wasm32-unknown-unknown --features octocrab,tower`;
//! it is never run, and it must not compile natively. Every test needs a
//! receiver or the octocrab model, so the file is empty without `http` or
//! `octocrab` rather than a set of orphaned definitions.

#![cfg(all(target_arch = "wasm32", any(feature = "http", feature = "octocrab")))]
// The handlers here bump a counter instead of awaiting a JavaScript binding,
// which is what a real `async fn handle` would do.
#![allow(clippy::unused_async_trait_impl)]

use std::{cell::Cell, rc::Rc};

use octoevents::{Envelope, Handler};

/// A Worker-shaped handler: holds a non-`Send`, non-`Sync` value.
struct Counter {
    calls: Rc<Cell<u32>>,
}

impl Handler<Envelope> for Counter {
    type Error = std::convert::Infallible;

    async fn handle(&self, _envelope: Envelope) -> Result<(), Self::Error> {
        self.calls.set(self.calls.get() + 1);
        Ok(())
    }
}

/// The application error a dispatcher under test converts every handler's
/// error into.
struct AppError;

impl From<octoevents::DecodeError> for AppError {
    fn from(_: octoevents::DecodeError) -> Self {
        Self
    }
}

impl From<std::convert::Infallible> for AppError {
    fn from(never: std::convert::Infallible) -> Self {
        match never {}
    }
}

#[cfg(feature = "http")]
#[test]
fn the_receiver_accepts_single_threaded_handler_state() {
    use octoevents::{Secret, Verifier, WebhookReceiverBuilder};

    let calls = Rc::new(Cell::new(0));
    let _receiver =
        WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(Counter {
            calls: Rc::clone(&calls),
        });

    let closure_calls = Rc::clone(&calls);
    let _receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
        move |_: Envelope| {
            let calls = Rc::clone(&closure_calls);
            async move {
                calls.set(calls.get() + 1);
                Ok::<_, ()>(())
            }
        },
    );
}

/// `receive` promises a `MaybeSend` future and asks the body for `MaybeSend`,
/// the bounds the `tower` `Service` impl places. On `wasm32` neither binds: a
/// Worker hands in an `http::Request<worker::Body>`, a JavaScript stream that
/// is neither `Send` nor `Sync`, to a handler holding `Rc` state.
#[cfg(feature = "http")]
#[test]
fn receive_accepts_a_single_threaded_body_and_handler() {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    use bytes::Bytes;
    use http::Request;
    use http_body::{Body, Frame};
    use octoevents::{Secret, Verifier, WebhookReceiverBuilder};

    /// A Worker-shaped body: holds a non-`Send`, non-`Sync` value.
    struct JsBody {
        stream: Rc<Cell<bool>>,
    }

    impl Body for JsBody {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            self.stream.set(true);
            Poll::Ready(None)
        }
    }

    let receiver =
        WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(Counter {
            calls: Rc::new(Cell::new(0)),
        });
    let request = Request::new(JsBody {
        stream: Rc::new(Cell::new(false)),
    });

    let _future = receiver.receive(request);
}

/// The `on_error` observer is bounded like a handler, so a Worker can record
/// failures into the same single-threaded state, JavaScript values included.
#[cfg(feature = "http")]
#[test]
fn the_receiver_accepts_a_single_threaded_error_observer() {
    use octoevents::{EventMeta, Secret, Verifier, WebhookReceiverBuilder};

    struct JsValue;

    let failures = Rc::new(Cell::new(0));
    let observer_failures = Rc::clone(&failures);
    let _receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
        .on_error(move |_: &EventMeta, _: &JsValue| {
            observer_failures.set(observer_failures.get() + 1);
        })
        .build(|_: Envelope| async { Err::<(), _>(JsValue) });
}

/// A handler over the envelope reaches the receiver through the dispatcher's
/// `always` and `fallback` tiers, and the erasure there must keep the relaxed
/// bound for the receiver to accept the dispatcher.
#[cfg(feature = "http")]
#[test]
fn the_receiver_accepts_a_dispatcher_over_single_threaded_always_and_fallback_handlers() {
    use octoevents::{Dispatcher, Secret, Verifier, WebhookReceiverBuilder};

    let calls = Rc::new(Cell::new(0));
    let closure_calls = Rc::clone(&calls);
    let dispatcher = Dispatcher::<AppError>::builder()
        .always(Counter {
            calls: Rc::clone(&calls),
        })
        .fallback(move |_: Envelope| {
            let calls = Rc::clone(&closure_calls);
            async move {
                calls.set(calls.get() + 1);
                Ok::<_, std::convert::Infallible>(())
            }
        })
        .build();
    let _receiver =
        WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);
}

/// The `tower_service::Service` impl boxes the handler's future, and that box
/// must drop `Send` on `wasm32` exactly as the handler trait does.
#[cfg(feature = "tower")]
#[test]
fn the_tower_service_impl_accepts_single_threaded_handler_state() {
    use bytes::Bytes;
    use http::Request;
    use http_body_util::Full;
    use octoevents::{Secret, Verifier, WebhookReceiverBuilder};
    use tower_service::Service;

    fn assert_service<S: Service<Request<Full<Bytes>>>>(_: &S) {}

    let receiver =
        WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(Counter {
            calls: Rc::new(Cell::new(0)),
        });
    assert_service(&receiver);
}

/// Handlers over the meta, the envelope and a consumer view reach the
/// dispatcher through `on` with no feature enabled, and the erasure keeps the
/// relaxed bound.
#[cfg(feature = "http")]
#[test]
fn the_dispatcher_accepts_single_threaded_handlers_over_the_meta_the_envelope_a_view_and_an_event()
{
    use octoevents::{
        Action, DecodeError, Dispatcher, Envelope, Event, EventKind, EventMeta, FromEnvelope,
        Secret, Verifier, WebhookReceiverBuilder,
    };

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

    struct Revoker {
        calls: Rc<Cell<u32>>,
    }

    impl Handler<EventMeta> for Revoker {
        type Error = std::convert::Infallible;

        async fn handle(&self, _meta: EventMeta) -> Result<(), Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    let calls = Rc::new(Cell::new(0));
    let closure_calls = Rc::clone(&calls);
    let view_calls = Rc::clone(&calls);
    let event_calls = Rc::clone(&calls);
    let dispatcher = Dispatcher::<AppError>::builder()
        .on(
            (EventKind::Installation, Action::Deleted),
            Revoker {
                calls: Rc::clone(&calls),
            },
        )
        .on(EventKind::Installation, move |_: EventMeta| {
            let calls = Rc::clone(&closure_calls);
            async move {
                calls.set(calls.get() + 1);
                Ok::<_, std::convert::Infallible>(())
            }
        })
        .on(
            EventKind::Push,
            Counter {
                calls: Rc::clone(&calls),
            },
        )
        .on(
            [EventKind::Issues, EventKind::IssueComment],
            move |sender: Sender| {
                let calls = Rc::clone(&view_calls);
                async move {
                    let _ = sender.sender.login;
                    calls.set(calls.get() + 1);
                    Ok::<_, std::convert::Infallible>(())
                }
            },
        )
        .on(
            [EventKind::Issues, EventKind::IssueComment],
            move |Event { meta, payload }: Event<Sender>| {
                let calls = Rc::clone(&event_calls);
                async move {
                    let _ = (meta.delivery_id, payload.sender.login);
                    calls.set(calls.get() + 1);
                    Ok::<_, std::convert::Infallible>(())
                }
            },
        )
        .build();
    let _receiver =
        WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);
}

#[cfg(feature = "octocrab")]
#[test]
fn the_dispatcher_accepts_single_threaded_handler_state_over_every_input() {
    use octocrab::models::webhook_events::{WebhookEvent, payload::PullRequestWebhookEventPayload};
    use octoevents::{Action, Dispatcher, Event, EventKind};

    struct Auditor {
        calls: Rc<Cell<u32>>,
    }

    impl Handler<Event<WebhookEvent>> for Auditor {
        type Error = std::convert::Infallible;

        async fn handle(&self, _event: Event<WebhookEvent>) -> Result<(), Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    struct Labeler {
        calls: Rc<Cell<u32>>,
    }

    impl Handler<PullRequestWebhookEventPayload> for Labeler {
        type Error = std::convert::Infallible;

        async fn handle(
            &self,
            _payload: PullRequestWebhookEventPayload,
        ) -> Result<(), Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    let calls = Rc::new(Cell::new(0));
    let closure_calls = Rc::clone(&calls);
    let dispatcher = Dispatcher::<AppError>::builder()
        .always(Counter {
            calls: Rc::clone(&calls),
        })
        .on(
            (
                EventKind::PullRequest,
                [Action::Opened, Action::Synchronize],
            ),
            move |_: WebhookEvent| {
                let calls = Rc::clone(&closure_calls);
                async move {
                    calls.set(calls.get() + 1);
                    Ok::<_, std::convert::Infallible>(())
                }
            },
        )
        .on(
            EventKind::Issues,
            Auditor {
                calls: Rc::clone(&calls),
            },
        )
        .on_payload(Labeler {
            calls: Rc::clone(&calls),
        })
        .fallback(Counter {
            calls: Rc::clone(&calls),
        })
        .build();

    #[cfg(feature = "http")]
    {
        use octoevents::{Secret, Verifier, WebhookReceiverBuilder};

        let _receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);
    }
    // Without `http` there is no receiver to hand it to; building it was the point.
    #[cfg(not(feature = "http"))]
    drop(dispatcher);
}
