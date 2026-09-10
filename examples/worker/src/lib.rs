//! The receiver on Cloudflare Workers: `WebhookReceiver::receive` over the
//! `http::Request` the `worker` crate hands over, built for
//! `wasm32-unknown-unknown`, as a GitHub App's receiver. `README.md` beside
//! this file says how to build, run and configure it; it is a package of its
//! own, outside the repository's workspace.
//!
//! The receiver is the same as on a native server. What differs is the
//! target: the crate's `MaybeSend` and `MaybeSync` bounds are empty on
//! `wasm32`, so a handler's future may await a JavaScript promise and its
//! error may hold a `JsValue`, and the crate's `BoxError` there is
//! `Box<dyn Error>`. The secret is read per request from the Worker's
//! bindings, so an empty one is reported as a value through
//! `str::parse::<WebhookSecret>` rather than the panic `WebhookSecret::new`
//! would raise, which would trap the wasm instance.
//!
//! Two handlers, in a dispatcher built without the `octocrab` feature (never
//! on by default):
//!
//! - [`InstallationLog`], routed with `on`, is a handler over
//!   `Event<InstallationView>`: the meta beside a consumer-defined view of
//!   the `installation` payload. The view declares its kind with
//!   `#[derive(Payload)]`, so the matcher, `AnyAction`, says only that every
//!   action of that kind is wanted.
//! - [`Forward`], in the `always` tier, is the part specific to this
//!   deployment: it serializes each envelope the dispatcher is handed as the
//!   crate's
//!   [wire format](https://docs.rs/octoevents/latest/octoevents/struct.Envelope.html#wire-format),
//!   the flat JSON document `serde_json::to_string(&envelope)` produces, and
//!   POSTs it to a [Restate](https://restate.dev) virtual object keyed by
//!   installation ID, which another service reads back through serde. The
//!   key is what makes this an App's receiver: a delivery with no
//!   installation ID (a repository webhook's, or an App's
//!   `github_app_authorization`) has no object to go to and fails, so it
//!   shows as failed in GitHub for an operator to redeliver or discard
//!   (GitHub never redelivers on its own), and a deployment that wants
//!   those keeps them under another key. The `ping` GitHub sends on
//!   creating the webhook never
//!   reaches this tier: the receiver answers it itself under the default
//!   `handle_ping(false)`. A forwarder to any internal service has the same
//!   shape; only the URL and the key are Restate's.
//!
//! A native `cargo check`, or rust-analyzer left on the host target, reports
//! `Forward::handle`'s future as not `MaybeSend`, naming the `JsFuture`
//! inside the fetch: `MaybeSend` is `Send` on native targets and empty on
//! `wasm32`, and a JavaScript promise is `!Send`. Point the editor at the
//! target the build uses, as `.vscode/settings.json` in this directory does
//! (`"rust-analyzer.cargo.target": "wasm32-unknown-unknown"`), and the
//! diagnostic goes away; there is nothing to fix in the handler.

// `InstallationLog::handle` logs where a real one would await a database or
// the GitHub API. The lint expectation says so; delete it once every handler
// body has an `.await`.
#![expect(clippy::unused_async_trait_impl)]

use std::{convert::Infallible, error::Error as _};

use octoevents::{
    AnyAction, DispatchError, Dispatcher, Envelope, Event, EventKind, EventMeta, Handler, Verifier,
    WebhookReceiverBuilder, WebhookSecret, WebhookSecretError,
};
use worker::{
    Context, Env, Fetch, HttpRequest, Method, Request, RequestInit, console_error, console_log,
    event,
};

/// The forwarder's error: its serialization, and its fetch. A `worker::Error`
/// holds a `JsValue` and is not `Send`, which the crate's `BoxError` admits on
/// `wasm32`, where it is `Box<dyn Error>`; the dispatcher boxes this error
/// where the forwarder is registered.
#[derive(Debug, thiserror::Error)]
enum ForwardError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Worker(#[from] worker::Error),
}

/// Forwards the envelope to the Restate virtual object for its installation.
/// Registered in the dispatcher's `always` tier, it receives each envelope
/// the dispatcher is handed, bytes included, and runs before any routed
/// handler; a delivery with no installation ID to key on, or one the ingress
/// refused, fails and is not routed.
struct Forward {
    object_url: String,
}

impl Handler<Envelope> for Forward {
    type Error = ForwardError;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        let installation_id = envelope
            .meta
            .installation_id
            .ok_or_else(|| worker::Error::RustError("payload has no installation ID".into()))?;
        let endpoint = format!(
            "{}/{installation_id}/receive",
            self.object_url.trim_end_matches('/')
        );
        // The wire format: one flat JSON object, the meta's fields at the top
        // level beside `raw_payload` as base64, which the service on the
        // other end reads back into an `Envelope` through serde.
        let body = serde_json::to_string(&envelope)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&body)));
        init.headers.set("content-type", "application/json")?;

        let request = Request::new_with_init(&endpoint, &init)?;
        let response = Fetch::Request(request).send().await?;
        let status = response.status_code();
        if !(200..300).contains(&status) {
            return Err(worker::Error::RustError(format!("ingress returned {status}")).into());
        }

        Ok(())
    }
}

/// A consumer view over the `installation` payload: the one field this worker
/// wants beyond what `EventMeta` already carries. The kind it declares is the
/// kind the dispatcher routes its handler by.
#[derive(serde::Deserialize, octoevents::Payload)]
#[payload(EventKind::Installation)]
struct InstallationView {
    installation: Installation,
}

#[derive(serde::Deserialize)]
struct Installation {
    account: Account,
}

#[derive(serde::Deserialize)]
struct Account {
    login: String,
}

/// Logs installation lifecycle changes. Receives the meta and the decoded
/// view and no payload bytes; other kinds never reach it. Logging cannot
/// fail, and the error type says so; a decode failure is reported by the
/// dispatcher at this handler's registration, not through its error type.
struct InstallationLog;

impl Handler<Event<InstallationView>> for InstallationLog {
    type Error = Infallible;

    async fn handle(
        &self,
        Event { meta, payload }: Event<InstallationView>,
    ) -> Result<(), Self::Error> {
        console_log!(
            "{}: installation {:?} {:?} for {}",
            meta.delivery_id,
            meta.installation_id,
            meta.action,
            payload.installation.account.login,
        );
        Ok(())
    }
}

#[event(fetch)]
async fn fetch(
    request: HttpRequest,
    env: Env,
    _context: Context,
) -> worker::Result<impl worker::IntoResponse> {
    let secret = env.secret("GITHUB_WEBHOOK_SECRET")?.to_string();
    let object_url = env.var("RESTATE_OBJECT_URL")?.to_string();

    // The secret is read per request, so an empty one is reported as a value
    // the runtime turns into a response, not a panic that traps the wasm
    // instance.
    let secret: WebhookSecret = secret
        .parse()
        .map_err(|error: WebhookSecretError| worker::Error::RustError(error.to_string()))?;
    let verifier = Verifier::new(secret);

    let dispatcher = Dispatcher::builder()
        .always(Forward { object_url })
        .on(AnyAction, InstallationLog)
        .build();

    let receiver = WebhookReceiverBuilder::new(verifier)
        .on_error(|meta: &EventMeta, error: &DispatchError| {
            console_error!("{}: {error}", meta.delivery_id);
            let mut cause = error.source();
            while let Some(error) = cause {
                console_error!("{}: caused by: {error}", meta.delivery_id);
                cause = error.source();
            }
        })
        .build(dispatcher);

    Ok(receiver.receive(request).await)
}
