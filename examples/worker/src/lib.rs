//! A Cloudflare Worker that forwards every verified envelope to a Restate
//! virtual object from the dispatcher's raw tier, then routes it.
//!
//! Built with `default-features = false`: no octocrab, so the dispatcher
//! routes raw, meta and payload handlers only, and the payload handler decodes
//! a consumer-defined view of the `installation` payload.

// The handlers here log instead of awaiting a database or the GitHub API,
// which is what a real `async fn handle` would do.
#![allow(clippy::unused_async_trait_impl)]

use std::convert::Infallible;

use octoevents::{
    DecodeError, Dispatcher, Envelope, EventKind, EventMeta, PayloadHandler, Secret, Verifier,
    WebhookHandler, WebhookReceiverBuilder,
};
use worker::{Context, Env, Fetch, HttpRequest, Method, Request, RequestInit, console_log, event};

/// The application error every handler's error converts into: the
/// dispatcher's payload decodes, the forwarder's serialization, and its fetch.
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Worker(#[from] worker::Error),
}

impl From<Infallible> for AppError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}

/// Forwards the raw envelope to the Restate ingress. Registered in the
/// dispatcher's raw tier, it runs before any typed handler, and a delivery the
/// ingress refused is not routed.
struct Forward {
    object_url: String,
}

impl WebhookHandler for Forward {
    type Error = AppError;

    async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
        let installation_id = envelope
            .meta
            .installation_id
            .ok_or_else(|| worker::Error::RustError("payload has no installation ID".into()))?;
        let endpoint = format!(
            "{}/{installation_id}/receive",
            self.object_url.trim_end_matches('/')
        );
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
#[derive(serde::Deserialize)]
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

octoevents::impl_payload!(InstallationView => EventKind::Installation);

/// Logs installation lifecycle changes. Receives the decoded view and no raw
/// bytes; other kinds never reach it.
struct InstallationLog;

impl PayloadHandler<InstallationView> for InstallationLog {
    type Error = Infallible;

    async fn handle(&self, meta: EventMeta, payload: InstallationView) -> Result<(), Self::Error> {
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

    let dispatcher = Dispatcher::<AppError>::builder()
        .always_raw(Forward { object_url })
        .handle_with(InstallationLog)
        .build();

    let receiver =
        WebhookReceiverBuilder::new(Verifier::new(Secret::new(secret))).build(dispatcher);

    Ok(receiver.receive(request).await)
}
