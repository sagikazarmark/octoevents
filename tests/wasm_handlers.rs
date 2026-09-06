//! Compile-time coverage for non-`Send` handler state on `wasm32`.
//!
//! Cloudflare Workers are single-threaded and hand handlers JavaScript values
//! and `Rc` state. `MaybeSend`/`MaybeSync` relax the handler bounds there, and
//! this file proves both handler flavours compile through the full erasure
//! path with such state. Build it with
//! `cargo build --test wasm_handlers --target wasm32-unknown-unknown --features octocrab,tower`;
//! it is never run, and it must not compile natively.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use octoevents::{Envelope, EventMeta, WebhookHandler};

/// A Worker-shaped handler: holds a non-`Send`, non-`Sync` value.
struct Counter {
    calls: Rc<Cell<u32>>,
}

impl WebhookHandler for Counter {
    type Error = std::convert::Infallible;

    async fn handle(&self, _envelope: Envelope) -> Result<(), Self::Error> {
        self.calls.set(self.calls.get() + 1);
        Ok(())
    }
}

/// The application error a dispatcher under test converts every handler's
/// error into.
#[cfg(any(feature = "http", feature = "octocrab"))]
struct AppError;

#[cfg(any(feature = "http", feature = "octocrab"))]
impl From<octoevents::DecodeError> for AppError {
    fn from(_: octoevents::DecodeError) -> Self {
        Self
    }
}

#[cfg(any(feature = "http", feature = "octocrab"))]
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

/// The `on_error` observer is bounded like a handler, so a Worker can record
/// failures into the same single-threaded state, JavaScript values included.
#[cfg(feature = "http")]
#[test]
fn the_receiver_accepts_a_single_threaded_error_observer() {
    use octoevents::{Secret, Verifier, WebhookReceiverBuilder};

    struct JsValue;

    let failures = Rc::new(Cell::new(0));
    let observer_failures = Rc::clone(&failures);
    let _receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
        .on_error(move |_: &EventMeta, _: &JsValue| {
            observer_failures.set(observer_failures.get() + 1);
        })
        .build(|_: Envelope| async { Err::<(), _>(JsValue) });
}

/// A webhook handler reaches the receiver through the dispatcher's `always`
/// and `fallback` tiers, and the erasure there must keep the relaxed bound
/// for the receiver to accept the dispatcher.
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
/// must drop `Send` on `wasm32` exactly as the handler traits do.
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

/// An event handler over `()` reaches the dispatcher through `on` with no
/// feature enabled, and the erasure keeps the relaxed bound.
#[cfg(feature = "http")]
#[test]
fn the_dispatcher_accepts_a_single_threaded_handler_over_unit() {
    use octoevents::{
        Action, Dispatcher, EventHandler, EventKind, EventMeta, Secret, Verifier,
        WebhookReceiverBuilder,
    };

    struct Revoker {
        calls: Rc<Cell<u32>>,
    }

    impl EventHandler<()> for Revoker {
        type Error = std::convert::Infallible;

        async fn handle(&self, _meta: EventMeta, (): ()) -> Result<(), Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    let calls = Rc::new(Cell::new(0));
    let closure_calls = Rc::clone(&calls);
    let dispatcher = Dispatcher::<AppError>::builder()
        .on(
            (EventKind::Installation, Action::Deleted),
            Revoker {
                calls: Rc::clone(&calls),
            },
        )
        .on(EventKind::Installation, move |_: EventMeta, (): ()| {
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

#[cfg(feature = "octocrab")]
#[test]
fn the_dispatcher_accepts_single_threaded_handler_state_of_both_flavours() {
    use octocrab::models::webhook_events::{WebhookEvent, payload::PullRequestWebhookEventPayload};
    use octoevents::{Action, Dispatcher, EventHandler, EventKind, EventMeta};

    struct Auditor {
        calls: Rc<Cell<u32>>,
    }

    impl EventHandler<WebhookEvent> for Auditor {
        type Error = std::convert::Infallible;

        async fn handle(&self, _meta: EventMeta, _event: WebhookEvent) -> Result<(), Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    struct Labeler {
        calls: Rc<Cell<u32>>,
    }

    impl EventHandler<PullRequestWebhookEventPayload> for Labeler {
        type Error = std::convert::Infallible;

        async fn handle(
            &self,
            _meta: EventMeta,
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
            move |_: EventMeta, _: WebhookEvent| {
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
}
