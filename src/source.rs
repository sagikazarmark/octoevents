use std::{future::Future, sync::Arc};

use crate::{MaybeSend, MaybeSync, Verifier, WebhookMeta};

/// What a receiver asks, per request and before verifying it, for the
/// [`Verifier`] of that request: one webhook URL serving several GitHub Apps,
/// each signing with its own secret.
///
/// The receiver reads the request's [`WebhookMeta`], hands it here, and
/// verifies the body against the verifier it gets back; a source that has
/// none answers `None`, and the request is refused as
/// [`ReceiveError::UnknownTarget`](crate::ReceiveError::UnknownTarget) (401)
/// before verification: on `receive`, before the body is read from the
/// transport, so the receiver buffers nothing for it; on `receive_bytes`, the
/// bytes being the caller's already, before the HMAC is computed over them.
/// A source typically keys by the target,
/// [`WebhookMeta::target_type`] and [`WebhookMeta::target_id`]: for a GitHub
/// App, `integration` and the App ID.
///
/// A [`Verifier`] is a source, answering itself for every request, which is
/// what [`WebhookReceiverBuilder::new`](crate::WebhookReceiverBuilder::new)
/// builds on; [`WebhookReceiverBuilder::from_source`](crate::WebhookReceiverBuilder::from_source)
/// takes any other. So is a closure over `&WebhookMeta` returning
/// `Option<Verifier>`, for a lookup in memory, and an `Arc` of a source, for
/// a registry shared with the rest of the application. A lookup that awaits,
/// a secret manager or a database, implements the trait on a struct:
///
/// ```
/// use std::collections::HashMap;
///
/// use octoevents::{WebhookMeta, TargetType, Verifier, VerifierSource, WebhookSecret};
///
/// /// The deployment's GitHub Apps, by App ID.
/// struct Apps {
///     verifiers: HashMap<u64, Verifier>,
/// }
///
/// impl VerifierSource for Apps {
///     async fn verifier(&self, headers: &WebhookMeta) -> Option<Verifier> {
///         // A secret manager would be awaited here.
///         match (headers.target_type.as_ref(), headers.target_id) {
///             (Some(TargetType::Integration), Some(id)) => self.verifiers.get(&id).cloned(),
///             _ => None,
///         }
///     }
/// }
///
/// let apps = Apps {
///     verifiers: HashMap::from([
///         (1, Verifier::new(WebhookSecret::new("first app's secret"))),
///         (2, Verifier::new(WebhookSecret::new("second app's secret"))),
///     ]),
/// };
/// # let _ = apps;
/// ```
///
/// # Security
///
/// The headers a source reads are not signed: GitHub's signature covers the
/// body alone. Choosing a secret by them is safe all the same. A forged
/// target selects a secret its sender does not know, so the body does not
/// verify and the request is refused; a delivery that does verify is
/// authentic for the secret that was selected, and for nothing else.
///
/// Attributing it to the target it claims, as the App it came from, takes two
/// things verification cannot see, both the source's to ensure:
///
/// - the source chose the verifier by that target, and answers each target
///   with that target's own secrets only. A source that ignores the target,
///   as a single [`Verifier`] does, or maps two targets to one verifier,
///   verifies App A's signed body under a forged App B header, and the
///   target stays a claim;
/// - no two targets share a secret, rotation windows included.
///
/// A lookup that fails, a secret manager that cannot be reached, answers as
/// a target the source does not know. GitHub records the 401 and the delivery
/// can be redelivered once the source recovers.
///
/// The source runs before verification, so it runs for requests from anyone
/// who can reach the URL, whether or not they know a secret. A source that
/// asks a remote service per request puts that service on the path of
/// unauthenticated traffic, where a flood of forged requests slows the
/// deliveries of every App behind the URL. Keep the secrets in memory or
/// cache them, bound the lookup's time and concurrency, and do not answer an
/// unknown target with a request to the backend.
pub trait VerifierSource: MaybeSync {
    /// The verifier to authenticate the request whose headers read as
    /// `headers`, or `None` to refuse it.
    fn verifier(&self, headers: &WebhookMeta)
    -> impl Future<Output = Option<Verifier>> + MaybeSend;
}

/// The one verifier for every request: a receiver with one secret, or one
/// rotation window.
impl VerifierSource for Verifier {
    fn verifier(
        &self,
        _headers: &WebhookMeta,
    ) -> impl Future<Output = Option<Verifier>> + MaybeSend {
        // Clones share the secrets, so this is a reference count.
        std::future::ready(Some(self.clone()))
    }
}

// `do_not_recommend` keeps rustc from explaining a missing impl as "the trait
// `Fn(&WebhookMeta)` is not implemented" for a type that was never meant to be
// a closure. `Fn` is fundamental, so this blanket does not overlap the
// `Verifier` and `Arc` impls, as it does not for `Handler`.
#[diagnostic::do_not_recommend]
impl<F> VerifierSource for F
where
    F: Fn(&WebhookMeta) -> Option<Verifier> + MaybeSync,
{
    fn verifier(
        &self,
        headers: &WebhookMeta,
    ) -> impl Future<Output = Option<Verifier>> + MaybeSend {
        std::future::ready(self(headers))
    }
}

// `Arc<S>` is `Sync` only when `S` is `Send` as well as `Sync`; the trait
// supplies `MaybeSync`, and `MaybeSend` is what the builder requires of `S`
// anyway.
impl<S: VerifierSource + MaybeSend> VerifierSource for Arc<S> {
    fn verifier(
        &self,
        headers: &WebhookMeta,
    ) -> impl Future<Output = Option<Verifier>> + MaybeSend {
        S::verifier(self, headers)
    }
}
