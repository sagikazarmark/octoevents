//! Compile-time coverage for non-`Send` handler state on `wasm32`.
//!
//! Cloudflare Workers are single-threaded and hand handlers JavaScript values
//! and `Rc` state. `MaybeSend`/`MaybeSync` relax the handler bounds there, and
//! this file proves every handler flavour compiles through the full erasure
//! path with such state. Build it with
//! `cargo build --test wasm_handlers --target wasm32-unknown-unknown --features octocrab,tower`;
//! it is never run, and it must not compile natively.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use octoevents::{Envelope, EventMeta, MetaHandler, WebhookHandler};

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

/// The same state behind a meta handler, which never sees the bytes.
struct MetaCounter {
    calls: Rc<Cell<u32>>,
}

impl MetaHandler for MetaCounter {
    type Error = std::convert::Infallible;

    async fn handle(&self, _meta: EventMeta) -> Result<(), Self::Error> {
        self.calls.set(self.calls.get() + 1);
        Ok(())
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

/// The meta adapter returns the handler's future as is, so the relaxed bound
/// must survive `into_webhook_handler()` into the receiver.
#[cfg(feature = "http")]
#[test]
fn the_receiver_accepts_a_single_threaded_meta_handler() {
    use octoevents::{Secret, Verifier, WebhookReceiverBuilder};

    let calls = Rc::new(Cell::new(0));
    let _receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
        MetaCounter {
            calls: Rc::clone(&calls),
        }
        .into_webhook_handler(),
    );

    let closure_calls = Rc::clone(&calls);
    let _receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
        (move |_: EventMeta| {
            let calls = Rc::clone(&closure_calls);
            async move {
                calls.set(calls.get() + 1);
                Ok::<_, ()>(())
            }
        })
        .into_webhook_handler(),
    );
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

#[cfg(feature = "octocrab")]
#[test]
fn the_dispatcher_accepts_single_threaded_handler_state_of_every_flavour() {
    use octocrab::models::webhook_events::{WebhookEvent, payload::PullRequestWebhookEventPayload};
    use octoevents::{
        Action, DecodeError, Dispatcher, EventHandler, EventKind, EventMeta, PayloadHandler,
    };

    struct AppError;
    impl From<DecodeError> for AppError {
        fn from(_: DecodeError) -> Self {
            Self
        }
    }
    impl From<std::convert::Infallible> for AppError {
        fn from(never: std::convert::Infallible) -> Self {
            match never {}
        }
    }

    struct Auditor {
        calls: Rc<Cell<u32>>,
    }

    impl EventHandler for Auditor {
        type Error = std::convert::Infallible;

        async fn handle(&self, _meta: EventMeta, _event: WebhookEvent) -> Result<(), Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    struct Labeler {
        calls: Rc<Cell<u32>>,
    }

    impl PayloadHandler<PullRequestWebhookEventPayload> for Labeler {
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
        .always(Auditor {
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
        .handle_with(Labeler {
            calls: Rc::clone(&calls),
        })
        .fallback(Auditor {
            calls: Rc::clone(&calls),
        })
        .build();

    // The typed flavours also reach the receiver directly.
    let _event_handler = Auditor {
        calls: Rc::clone(&calls),
    }
    .into_webhook_handler();
    let _payload_handler = Labeler {
        calls: Rc::clone(&calls),
    }
    .into_webhook_handler();

    #[cfg(feature = "http")]
    {
        use octoevents::{Secret, Verifier, WebhookReceiverBuilder};

        let _receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);
    }
}
