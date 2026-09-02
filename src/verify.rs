use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::Secret;

const SHA256_PREFIX: &str = "sha256=";
const SHA256_BYTES: usize = 32;
const SHA256_HEX_CHARS: usize = SHA256_BYTES * 2;

/// A failure to authenticate a webhook body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum VerifyError {
    /// The `X-Hub-Signature-256` header was absent.
    ///
    /// Envelope constructors produce this variant while extracting headers;
    /// [`Verifier::verify`] itself accepts an already-extracted header value.
    #[error("missing X-Hub-Signature-256 header")]
    MissingSignature,
    /// The signature was not `sha256=` followed by exactly 64 hexadecimal characters.
    #[error("malformed X-Hub-Signature-256 header")]
    MalformedSignature,
    /// None of the configured secrets matched the signature.
    #[error("webhook signature mismatch")]
    Mismatch,
}

/// The configured secrets, and the HMAC comparison they authenticate with.
///
/// A verifier is required to receive a webhook: it is constructed from one
/// secret, so a deployment without a secret cannot be expressed. Additional
/// secrets are added with [`Verifier::also`] to open a client-side rotation
/// window.
///
/// Clones share the secrets rather than copying them: a receiver clones its
/// verifier per delivery, and secret material should not be re-copied onto the
/// heap that often.
///
/// ```
/// use octoevents::{Secret, Verifier};
///
/// let verifier = Verifier::new(Secret::new("current secret"))
///     .also(Secret::new("previous secret"));
/// # let _ = verifier;
/// ```
#[derive(Debug, Clone)]
pub struct Verifier {
    secrets: Arc<Vec<Secret>>,
}

impl Verifier {
    /// Creates a verifier authenticating against one secret.
    ///
    /// # Panics
    ///
    /// Panics when the secret is empty. An empty secret is the unset- or
    /// mistyped-environment-variable failure mode, not a configuration, and
    /// every delivery would verify against a guessable key.
    #[must_use]
    #[track_caller]
    pub fn new(secret: Secret) -> Self {
        Self {
            secrets: Arc::new(vec![require_non_empty(secret)]),
        }
    }

    /// Accepts a further secret, for client-side credential rotation.
    ///
    /// GitHub configures exactly one secret per webhook, so a rotation window
    /// lives here: accept the new secret alongside the old one, change it in
    /// the App settings, then drop the old one once in-flight deliveries drain.
    ///
    /// # Panics
    ///
    /// Panics when the secret is empty, as [`Verifier::new`] does.
    #[must_use]
    #[track_caller]
    pub fn also(mut self, secret: Secret) -> Self {
        Arc::make_mut(&mut self.secrets).push(require_non_empty(secret));
        self
    }

    /// Verifies a GitHub HMAC-SHA256 signature over the exact body bytes.
    ///
    /// Every secret is evaluated even after a match. This provides
    /// authentication but not replay protection; deduplicate downstream using
    /// `X-GitHub-Delivery`.
    ///
    /// The `sha256=` prefix is matched case-sensitively because that is what
    /// GitHub sends; the hexadecimal digits after it accept either case. An
    /// uppercase prefix is [`VerifyError::MalformedSignature`], not a mismatch.
    ///
    /// ```
    /// use octoevents::{Secret, Verifier};
    ///
    /// let verifier = Verifier::new(Secret::new("It's a Secret to Everybody"));
    /// let signature = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
    /// verifier.verify(signature, b"Hello, World!")?;
    /// # Ok::<(), octoevents::VerifyError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError::MalformedSignature`] for any value not matching
    /// GitHub's `sha256=<64 hex characters>` format and [`VerifyError::Mismatch`]
    /// when no secret matches.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            name = "octoevents.verify",
            skip_all,
            fields(secret_count = self.secrets.len(), body_len = body.len(), outcome = tracing::field::Empty)
        )
    )]
    pub fn verify(&self, signature_header: &str, body: &[u8]) -> Result<(), VerifyError> {
        let received = match decode_signature(signature_header) {
            Ok(received) => received,
            Err(error) => {
                record_outcome("malformed");
                return Err(error);
            }
        };
        let mut matched = 0_u8;

        for secret in self.secrets.iter() {
            let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.expose()) else {
                continue;
            };
            mac.update(body);
            let computed = mac.finalize().into_bytes();
            matched |= computed[..].ct_eq(&received).unwrap_u8();
        }

        if matched == 1 {
            record_outcome("verified");
            Ok(())
        } else {
            record_outcome("mismatch");
            Err(VerifyError::Mismatch)
        }
    }
}

#[track_caller]
fn require_non_empty(secret: Secret) -> Secret {
    assert!(
        !secret.expose().is_empty(),
        "webhook secret must not be empty"
    );
    secret
}

fn record_outcome(outcome: &'static str) {
    #[cfg(feature = "tracing")]
    tracing::Span::current().record("outcome", outcome);
    let _ = outcome;
}

fn decode_signature(value: &str) -> Result<[u8; SHA256_BYTES], VerifyError> {
    let hex = value
        .strip_prefix(SHA256_PREFIX)
        .ok_or(VerifyError::MalformedSignature)?;
    if hex.len() != SHA256_HEX_CHARS {
        return Err(VerifyError::MalformedSignature);
    }

    // The length check above guarantees an exact number of pairs and no remainder.
    let (pairs, _) = hex.as_bytes().as_chunks::<2>();
    let mut decoded = [0_u8; SHA256_BYTES];
    for (output, &[high, low]) in decoded.iter_mut().zip(pairs) {
        let high = hex_nibble(high).ok_or(VerifyError::MalformedSignature)?;
        let low = hex_nibble(low).ok_or(VerifyError::MalformedSignature)?;
        *output = (high << 4) | low;
    }
    Ok(decoded)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Verifier, VerifyError};
    use crate::Secret;

    const DOCUMENTED_SIGNATURE: &str =
        "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";

    #[test]
    fn accepts_githubs_documented_test_vector() {
        let verifier = Verifier::new(Secret::new("It's a Secret to Everybody"));

        assert_eq!(
            verifier.verify(DOCUMENTED_SIGNATURE, b"Hello, World!"),
            Ok(())
        );
    }

    #[test]
    fn accepts_uppercase_hex() {
        let verifier = Verifier::new(Secret::new("It's a Secret to Everybody"));

        assert_eq!(
            verifier.verify(
                &DOCUMENTED_SIGNATURE
                    .to_uppercase()
                    .replacen("SHA256", "sha256", 1),
                b"Hello, World!"
            ),
            Ok(())
        );
    }

    #[test]
    fn tries_every_configured_secret() {
        let verifier =
            Verifier::new(Secret::new("wrong")).also(Secret::new("It's a Secret to Everybody"));

        assert_eq!(
            verifier.verify(DOCUMENTED_SIGNATURE, b"Hello, World!"),
            Ok(())
        );
    }

    #[test]
    fn clones_share_the_secrets_until_one_of_them_adds_another() {
        let original = Verifier::new(Secret::new("It's a Secret to Everybody"));
        let clone = original.clone();
        assert!(Arc::ptr_eq(&original.secrets, &clone.secrets));

        let extended = clone.also(Secret::new("wrong"));
        assert_eq!(original.secrets.len(), 1);
        assert_eq!(extended.secrets.len(), 2);
        assert_eq!(
            original.verify(DOCUMENTED_SIGNATURE, b"Hello, World!"),
            Ok(())
        );
    }

    #[test]
    #[should_panic(expected = "webhook secret must not be empty")]
    fn rejects_an_empty_secret_at_construction() {
        let _ = Verifier::new(Secret::new(""));
    }

    #[test]
    #[should_panic(expected = "webhook secret must not be empty")]
    fn rejects_an_empty_rotation_secret() {
        let _ = Verifier::new(Secret::new("secret")).also(Secret::new(""));
    }

    #[test]
    fn rejects_wrong_secrets() {
        let verifier = Verifier::new(Secret::new("wrong")).also(Secret::new("also wrong"));

        assert_eq!(
            verifier.verify(DOCUMENTED_SIGNATURE, b"Hello, World!"),
            Err(VerifyError::Mismatch)
        );
    }

    #[test]
    fn rejects_malformed_signatures_distinctly() {
        let verifier = Verifier::new(Secret::new("secret"));
        for signature in [
            "",
            "sha1=757107ea0eb2509fc211221cce984b8a37570b6d",
            "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
            "sha256=757107ea",
            "sha256=z57107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
            // The prefix is deliberately case-sensitive: GitHub sends lowercase.
            "SHA256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
        ] {
            assert_eq!(
                verifier.verify(signature, b"Hello, World!"),
                Err(VerifyError::MalformedSignature),
                "signature: {signature}"
            );
        }
    }

    #[test]
    fn empty_body_is_still_authenticated() {
        let verifier = Verifier::new(Secret::new("secret"));
        let signature = "sha256=f9e66e179b6747ae54108f82f8ade8b3c25d76fd30afde6c395822c530196169";

        assert_eq!(verifier.verify(signature, b""), Ok(()));
    }

    #[test]
    fn debug_output_redacts_the_secrets() {
        let verifier = Verifier::new(Secret::new("super-secret"));

        let debug = format!("{verifier:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret"));
    }
}
