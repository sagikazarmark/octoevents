//! `trace_errors` on a receiver whose handler returns the front page's
//! `Box<dyn Error + Send + Sync>`: the error the receiver holds is the
//! `DispatchError` over it, which is not an `Error`, and the diagnostic must
//! say so in the crate's words and name `trace_boxed_errors` in its note.
use octoevents::{Dispatcher, Verifier, WebhookReceiverBuilder, WebhookSecret};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn main() {
    let dispatcher = Dispatcher::<BoxError>::builder().build();
    let _receiver = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("development-secret")))
        .trace_errors()
        .build(dispatcher);
}
