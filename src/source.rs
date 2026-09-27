use std::{future::Future, sync::Arc};

use crate::{HeaderMeta, MaybeSend, MaybeSync, Verifier};

/// What a receiver asks, per request and before reading the body, for the
/// [`Verifier`] of that request: one webhook URL serving several GitHub Apps,
/// each signing with its own secret.
///
/// The receiver reads the request's [`HeaderMeta`], hands it here, and
/// verifies the body against the verifier it gets back; a source that has
/// none answers `None`, and the request is refused as
/// [`ReceiveError::UnknownTarget`](crate::ReceiveError::UnknownTarget) (401)
/// without its body being read. A source typically keys by
/// [`HeaderMeta::target`]: for a GitHub App, the App ID.
///
/// A [`Verifier`] is a source, answering itself for every request, which is
/// what [`WebhookReceiverBuilder::new`](crate::WebhookReceiverBuilder::new)
/// builds on; [`WebhookReceiverBuilder::from_source`](crate::WebhookReceiverBuilder::from_source)
/// takes any other. So is a closure over `&HeaderMeta` returning
/// `Option<Verifier>`, for a lookup in memory, and an `Arc` of a source, for
/// a registry shared with the rest of the application. A lookup that awaits,
/// a secret manager or a database, implements the trait on a struct:
///
/// ```
/// use std::collections::HashMap;
///
/// use octoevents::{HeaderMeta, Target, TargetType, Verifier, VerifierSource, WebhookSecret};
///
/// /// The deployment's GitHub Apps, by App ID.
/// struct Apps {
///     verifiers: HashMap<u64, Verifier>,
/// }
///
/// impl VerifierSource for Apps {
///     async fn verifier(&self, headers: &HeaderMeta) -> Option<Verifier> {
///         // A secret manager would be awaited here.
///         match headers.target.as_ref()? {
///             Target { kind: TargetType::Integration, id } => self.verifiers.get(id).cloned(),
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
/// authentic for the secret that was selected. Attributing it to that
/// target, as the App it came from, is sound only when no two targets share
/// a secret, which is the source's to ensure: verification cannot see it.
///
/// A lookup that fails, a secret manager that cannot be reached, answers as
/// a target the source does not know. GitHub records the 401 and the delivery
/// can be redelivered once the source recovers.
pub trait VerifierSource: MaybeSync {
    /// The verifier to authenticate the request whose headers read as
    /// `headers`, or `None` to refuse it.
    fn verifier(&self, headers: &HeaderMeta) -> impl Future<Output = Option<Verifier>> + MaybeSend;
}

/// The one verifier for every request: a receiver with one secret, or one
/// rotation window.
impl VerifierSource for Verifier {
    fn verifier(
        &self,
        _headers: &HeaderMeta,
    ) -> impl Future<Output = Option<Verifier>> + MaybeSend {
        // Clones share the secrets, so this is a reference count.
        std::future::ready(Some(self.clone()))
    }
}

// `do_not_recommend` keeps rustc from explaining a missing impl as "the trait
// `Fn(&HeaderMeta)` is not implemented" for a type that was never meant to be
// a closure. `Fn` is fundamental, so this blanket does not overlap the
// `Verifier` and `Arc` impls, as it does not for `Handler`.
#[diagnostic::do_not_recommend]
impl<F> VerifierSource for F
where
    F: Fn(&HeaderMeta) -> Option<Verifier> + MaybeSync,
{
    fn verifier(&self, headers: &HeaderMeta) -> impl Future<Output = Option<Verifier>> + MaybeSend {
        std::future::ready(self(headers))
    }
}

// `Arc<S>` is `Sync` only when `S` is `Send` as well as `Sync`; the trait
// supplies `MaybeSync`, and `MaybeSend` is what the builder requires of `S`
// anyway.
impl<S: VerifierSource + MaybeSend> VerifierSource for Arc<S> {
    fn verifier(&self, headers: &HeaderMeta) -> impl Future<Output = Option<Verifier>> + MaybeSend {
        S::verifier(self, headers)
    }
}
