use octoevents::{Secret, Verifier, WebhookReceiverBuilder};
use worker::{Context, Env, Fetch, HttpRequest, Method, Request, RequestInit, event};

#[event(fetch)]
async fn fetch(
    request: HttpRequest,
    env: Env,
    _context: Context,
) -> worker::Result<impl worker::IntoResponse> {
    let secret = env.secret("GITHUB_WEBHOOK_SECRET")?.to_string();
    let object_url = env.var("RESTATE_OBJECT_URL")?.to_string();

    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(secret)))
        .build(move |envelope: octoevents::Envelope| {
            let object_url = object_url.clone();
            async move {
                let installation_id = envelope.common.installation_id.ok_or_else(|| {
                    worker::Error::RustError("payload has no installation ID".into())
                })?;
                let endpoint = format!(
                    "{}/{installation_id}/receive",
                    object_url.trim_end_matches('/')
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
                    return Err(worker::Error::RustError(format!(
                        "ingress returned {status}"
                    )));
                }

                Ok::<_, worker::Error>(())
            }
        });

    Ok(receiver.receive(request).await)
}
