use bytes::Bytes;
use http::HeaderMap;

use crate::{Envelope, WebhookMeta, ReceiveError, Verifier, header};

/// Authenticates a request and builds its envelope: the one path from an
/// untrusted request to an [`Envelope`] that can be trusted.
///
/// The receiver is built on it, and a transport calls it directly when it
/// wants the envelope and not the receiver's answer: to forward the envelope
/// over the wire format, to persist it before any handler runs, or to route
/// it itself. An [`Envelope`] is data, and nothing about one says it was
/// authenticated; that it came from here, or from the receiver, is what does.
///
/// It takes the request's `http::HeaderMap` and the body as [`Bytes`], the
/// shape every surveyed Rust runtime hands over. A consumer hand-parsing a
/// raw invocation event collects its `(name, value)` pairs into a
/// `HeaderMap`; header-name case is `HeaderName`'s to handle. A failure is
/// answered with [`ReceiveError::status`], the receiver's contract.
///
/// Verification authenticates only the payload bytes. GitHub's signature
/// does not cover the delivery ID, event name, or target headers, so their
/// metadata is not an authenticated authorization claim. Use authenticated
/// payload data or independently trusted configuration for authorization.
/// Delivery-ID deduplication handles GitHub redelivery, not an attacker
/// resubmitting a captured signed payload with a new ID.
///
/// For the whole contract over the same two arguments (the header-only
/// refusal, the body limit, the `ping` short-circuit, the handler and the
/// tracing), call
/// [`WebhookReceiver::receive_bytes`](crate::WebhookReceiver::receive_bytes)
/// instead, which is in the core beside this.
///
/// The headers are read by the names in [`header`](crate::header), by which
/// `HeaderMap` matches case-insensitively, and a repeated header reads as its
/// first value. The signature is parsed from the header value's bytes, so a
/// value that is not visible ASCII is
/// [`SignatureError::Malformed`](crate::SignatureError::Malformed), not
/// `Missing`; for every other header such a value reads as absent, and an
/// empty delivery ID or event name is [`ReceiveError::MissingHeader`] as an
/// absent one is.
///
/// Once the body is authenticated, the headers are read into a
/// [`WebhookMeta`], as [`WebhookMeta::from_headers`] reads them, and the
/// envelope is built from it and the body as [`Envelope::new`] builds one,
/// the payload probed by the same rules. A target type this crate does not
/// know is [`TargetType::Unknown`](crate::TargetType::Unknown) with the value
/// intact; a target ID that is present but not a number reads as `None`,
/// since the header is optional and refusing an authenticated delivery over
/// it would serve nothing. The body is not copied: it becomes
/// [`Envelope::raw_payload`] as it is.
///
/// The verifier is the caller's to choose. A transport serving several
/// GitHub Apps at one URL chooses it per request, as the receiver does with a
/// [`VerifierSource`](crate::VerifierSource): it reads the [`WebhookMeta`]
/// first, asks its source for the verifier of that target, and passes the
/// verifier here, answering a target it has no verifier for with
/// [`ReceiveError::UnknownTarget`].
///
/// The body must be `application/json`, which is a setting on the GitHub
/// webhook; anything else is [`ReceiveError::UnsupportedContentType`]. The
/// other setting, `application/x-www-form-urlencoded`, wraps the JSON in a
/// `payload` form parameter and signs the form body, so the signed input
/// would no longer be the payload every decode reads. Refusing it is what
/// lets [`Envelope::raw_payload`] be both.
///
/// In a test, the request is signed with the verifier the envelope is
/// checked against:
///
/// ```
/// use http::HeaderMap;
/// use octoevents::{Action, Bytes, EventKind, Verifier, WebhookSecret, authenticate, header};
///
/// let verifier = Verifier::new(WebhookSecret::new("test-secret"));
/// let body = Bytes::from_static(br#"{"action":"opened","installation":{"id":42}}"#);
///
/// let mut headers = HeaderMap::new();
/// headers.insert(header::CONTENT_TYPE, "application/json".parse()?);
/// headers.insert(header::DELIVERY_ID, "delivery-1".parse()?);
/// headers.insert(header::EVENT_NAME, "issues".parse()?);
/// headers.insert(header::SIGNATURE, verifier.sign(&body).into());
///
/// let envelope = authenticate(&verifier, &headers, body)?;
///
/// assert_eq!(envelope.meta.delivery_id, "delivery-1");
/// assert_eq!(envelope.meta.kind, EventKind::Issues);
/// assert_eq!(envelope.meta.action, Some(Action::Opened));
/// assert_eq!(envelope.meta.installation_id, Some(42));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # What the receiver adds
///
/// Around this call the receiver refuses a request whose signature header is
/// absent (401) or not a signature (400) from the headers alone: on
/// `receive`, before the body is read from the transport, so unsigned
/// traffic is never buffered by the receiver, and on `receive_bytes`, before
/// anything else, the bytes being the caller's already. It bounds the
/// payload length at the configured limit (413), and answers a verified
/// `ping` 204 before any handler runs, unless asked to `handle_ping`. A
/// transport calling this function directly does those for itself, or
/// decides to go without: without the first, unsigned traffic is buffered
/// before it is refused; without the second, this function verifies
/// whatever it is given; without the third, a transport that forwards every
/// envelope forwards pings too. The header check is `Signature::try_from` on
/// the [`header::SIGNATURE`](crate::header::SIGNATURE) value, answered with
/// [`ReceiveError::status`]:
///
/// ```
/// use http::{HeaderMap, StatusCode};
/// use octoevents::{
///     Bytes, Envelope, ReceiveError, Signature, SignatureError, Verifier, WebhookSecret,
///     authenticate, header,
/// };
///
/// fn envelope_or_status(
///     verifier: &Verifier,
///     headers: &HeaderMap,
///     body: Bytes,
/// ) -> Result<Envelope, StatusCode> {
///     // Decidable from the headers, so a transport that streams runs it
///     // before buffering; `authenticate` reaches the same answer after.
///     headers
///         .get(&header::SIGNATURE)
///         .ok_or(SignatureError::Missing)
///         .and_then(Signature::try_from)
///         .map_err(|error| ReceiveError::from(error).status())?;
///
///     authenticate(verifier, headers, body).map_err(|error| error.status())
/// }
///
/// // An unsigned request is refused at the header check, before the body is read.
/// let verifier = Verifier::new(WebhookSecret::new("test-secret"));
/// let unsigned = HeaderMap::new();
/// assert_eq!(
///     envelope_or_status(&verifier, &unsigned, Bytes::from_static(b"{}")).unwrap_err(),
///     StatusCode::UNAUTHORIZED
/// );
/// ```
///
/// # Errors
///
/// Returns a signature error first, [`ReceiveError::Signature`]:
/// [`SignatureError::Missing`](crate::SignatureError::Missing) when the
/// header is absent, [`SignatureError::Malformed`](crate::SignatureError::Malformed)
/// when it does not parse as a [`Signature`](crate::Signature), and
/// [`SignatureError::Mismatch`](crate::SignatureError::Mismatch) when no
/// configured secret produced it for `body`, in that order. Then, for an
/// authenticated request, [`ReceiveError::UnsupportedContentType`], and
/// last [`ReceiveError::MissingHeader`] for the delivery ID and then the
/// event name.
pub fn authenticate(
    verifier: &Verifier,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Envelope, ReceiveError> {
    let signature = header::signature(headers)?;
    verifier.verify(&signature, &body)?;

    // Refused only once authenticated, so an unsigned request is refused for
    // its signature whatever its content type.
    if !header::is_json(headers) {
        return Err(ReceiveError::UnsupportedContentType);
    }

    let meta = WebhookMeta::from_headers(headers)?;
    Ok(Envelope::from_bytes(meta, body))
}

#[cfg(test)]
mod tests;
