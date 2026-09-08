use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::{Secret, trace};

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

/// A secret the verifier refuses to be configured with.
///
/// Reported by [`Verifier::try_new`] and [`Verifier::try_also`]; the
/// panicking [`Verifier::new`] and [`Verifier::also`] panic with the same
/// message instead. This is a configuration failure, found before any
/// delivery arrives, and so is kept apart from [`VerifyError`], which
/// reports a body that did not authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum SecretError {
    /// The secret has no bytes.
    ///
    /// An empty secret is the unset- or mistyped-environment-variable failure
    /// mode, not a configuration: every delivery would verify against a
    /// guessable key.
    #[error("webhook secret must not be empty")]
    Empty,
}

/// The configured secrets, and the HMAC comparison they authenticate with.
///
/// A verifier is required to receive a webhook: it is constructed from one
/// secret, so a deployment without a secret cannot be expressed. Additional
/// secrets are added with [`Verifier::also`] to open a client-side rotation
/// window.
///
/// Clones share the secrets rather than copying them: a receiver holds its
/// verifier and borrows it per delivery, so the clone is for a verifier that
/// is handed to more than one receiver, or kept beside the one receiving so
/// a test can sign with it, without secret material being copied onto the
/// heap each time.
///
/// Only `X-Hub-Signature-256` is verified. GitHub sends the SHA-1
/// `X-Hub-Signature` beside it, and a request carrying only that header is
/// [`VerifyError::MissingSignature`]: the SHA-256 header is always there to
/// verify, so falling back to the SHA-1 one would protect no delivery and
/// would let a sender choose the weaker algorithm.
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
    /// For a deployment that reads its secret at startup, where an empty one
    /// should stop the process; [`Verifier::try_new`] reports the same
    /// failure as a value, for one that reads it per request.
    ///
    /// # Panics
    ///
    /// Panics when the secret is empty, with [`SecretError::Empty`]'s
    /// message. An empty secret is the unset- or mistyped-environment-variable
    /// failure mode, not a configuration, and every delivery would verify
    /// against a guessable key.
    #[must_use]
    #[track_caller]
    pub fn new(secret: Secret) -> Self {
        or_panic(Self::try_new(secret))
    }

    /// Creates a verifier authenticating against one secret, or reports why
    /// the secret was refused.
    ///
    /// The fallible counterpart of [`Verifier::new`], for a deployment that
    /// builds its verifier where a panic is the wrong answer: a serverless
    /// function reading its secret per request, say, where an unset variable
    /// should be a response and not a trap.
    ///
    /// ```
    /// use octoevents::{Secret, SecretError, Verifier};
    ///
    /// let verifier = Verifier::try_new(Secret::new("current secret"))?;
    /// # let _ = verifier;
    ///
    /// assert_eq!(Verifier::try_new(Secret::new("")).unwrap_err(), SecretError::Empty);
    /// # Ok::<(), SecretError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SecretError::Empty`] when the secret has no bytes.
    pub fn try_new(secret: Secret) -> Result<Self, SecretError> {
        Ok(Self {
            secrets: Arc::new(vec![require_non_empty(secret)?]),
        })
    }

    /// Accepts a further secret, for client-side credential rotation.
    ///
    /// GitHub configures exactly one secret per webhook, so a rotation window
    /// lives here: accept the new secret alongside the old one, change it in
    /// the App settings, then drop the old one once in-flight deliveries drain.
    /// [`Verifier::try_also`] is the same step reporting an empty secret as a
    /// value.
    ///
    /// # Panics
    ///
    /// Panics when the secret is empty, as [`Verifier::new`] does.
    #[must_use]
    #[track_caller]
    pub fn also(self, secret: Secret) -> Self {
        or_panic(self.try_also(secret))
    }

    /// Accepts a further secret, or reports why it was refused.
    ///
    /// The fallible counterpart of [`Verifier::also`], to pair with
    /// [`Verifier::try_new`].
    ///
    /// ```
    /// use octoevents::{Secret, SecretError, Verifier};
    ///
    /// let verifier = Verifier::try_new(Secret::new("current secret"))?
    ///     .try_also(Secret::new("previous secret"))?;
    /// # let _ = verifier;
    /// # Ok::<(), SecretError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SecretError::Empty`] when the secret has no bytes; the
    /// verifier is consumed either way.
    pub fn try_also(mut self, secret: Secret) -> Result<Self, SecretError> {
        Arc::make_mut(&mut self.secrets).push(require_non_empty(secret)?);
        Ok(self)
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
            level = "debug",
            skip_all,
            fields(secret_count = self.secrets.len(), body_len = body.len(), outcome = tracing::field::Empty)
        )
    )]
    pub fn verify(&self, signature_header: &str, body: &[u8]) -> Result<(), VerifyError> {
        let received = match decode_signature(signature_header) {
            Ok(received) => received,
            Err(error) => {
                trace::record("outcome", "malformed");
                return Err(error);
            }
        };
        let mut matched = 0_u8;

        for secret in self.secrets.iter() {
            let computed = hmac_sha256(secret, body);
            matched |= computed[..].ct_eq(&received).unwrap_u8();
        }

        if matched == 1 {
            trace::record("outcome", "verified");
            Ok(())
        } else {
            trace::record("outcome", "mismatch");
            Err(VerifyError::Mismatch)
        }
    }

    /// The `X-Hub-Signature-256` value GitHub would send for `body`: `sha256=`
    /// followed by the lowercase hex HMAC-SHA256 of the body under the first
    /// configured secret.
    ///
    /// A test aid for the receiving side. A test drives the receiver with a
    /// synthetic request, and that request needs the signature GitHub would
    /// have put on it, so the test signs the body with the verifier the
    /// receiver was built with. The crate sends nothing; the method exists so
    /// the test needs no HMAC code of its own.
    ///
    /// A rotated verifier ([`Verifier::also`]) signs under its first secret,
    /// the one [`Verifier::new`] received. To sign under a secret it was
    /// rotated to, build a verifier from that secret alone.
    ///
    /// Nothing is recorded: the signature is secret-derived, and the crate
    /// records neither the secret nor anything computed from it.
    ///
    /// ```
    /// use octoevents::{Secret, Verifier};
    ///
    /// let verifier = Verifier::new(Secret::new("It's a Secret to Everybody"));
    /// let signature = verifier.sign(b"Hello, World!");
    ///
    /// assert_eq!(signature, "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17");
    /// verifier.verify(&signature, b"Hello, World!")?;
    /// # Ok::<(), octoevents::VerifyError>(())
    /// ```
    #[must_use]
    pub fn sign(&self, body: &[u8]) -> String {
        // `new` puts one secret in and nothing takes one out.
        let tag = hmac_sha256(&self.secrets[0], body);
        encode_signature(&tag)
    }
}

/// The HMAC-SHA256 of `body` under `secret`.
fn hmac_sha256(secret: &Secret, body: &[u8]) -> [u8; SHA256_BYTES] {
    // HMAC takes a key of any length: a long one is hashed to fit and a short
    // one is padded, so the key is never invalid.
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.expose()).expect("HMAC accepts a key of any length");
    mac.update(body);
    mac.finalize().into_bytes().into()
}

/// The panicking constructors' unwrap: the error's message, reported at the
/// consumer's line. `#[track_caller]` here and on `new` and `also` is what
/// carries the location through; a closure would report this file.
#[track_caller]
fn or_panic(result: Result<Verifier, SecretError>) -> Verifier {
    match result {
        Ok(verifier) => verifier,
        Err(error) => panic!("{error}"),
    }
}

fn require_non_empty(secret: Secret) -> Result<Secret, SecretError> {
    if secret.expose().is_empty() {
        return Err(SecretError::Empty);
    }
    Ok(secret)
}

/// `sha256=` followed by `tag` as lowercase hex: the header value GitHub
/// sends, and the inverse of [`decode_signature`].
fn encode_signature(tag: &[u8; SHA256_BYTES]) -> String {
    let mut out = String::with_capacity(SHA256_PREFIX.len() + SHA256_HEX_CHARS);
    out.push_str(SHA256_PREFIX);
    for &byte in tag {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

/// The lowercase hex digit for a nibble; the inverse of [`hex_nibble`].
fn hex_digit(nibble: u8) -> char {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    char::from(DIGITS[usize::from(nibble & 0x0f)])
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

    use super::{SecretError, Verifier, VerifyError};
    use crate::Secret;

    /// GitHub's documented test vector: `Hello, World!` under `It's a
    /// Secret to Everybody`.
    const DOCUMENTED_SIGNATURE: &str =
        "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
    /// The empty body under `secret`.
    const EMPTY_BODY_SIGNATURE: &str =
        "sha256=f9e66e179b6747ae54108f82f8ade8b3c25d76fd30afde6c395822c530196169";

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
    fn try_new_reports_an_empty_secret_instead_of_panicking() {
        // The environment-variable failure mode as a value: a deployment that
        // reads its secret at request time answers instead of trapping.
        let error = Verifier::try_new(Secret::new("")).unwrap_err();

        assert_eq!(error, SecretError::Empty);
        assert_eq!(error.to_string(), "webhook secret must not be empty");
    }

    #[test]
    fn try_new_builds_the_verifier_new_would() {
        let verifier = Verifier::try_new(Secret::new("It's a Secret to Everybody")).unwrap();

        assert_eq!(
            verifier.verify(DOCUMENTED_SIGNATURE, b"Hello, World!"),
            Ok(())
        );
    }

    #[test]
    fn try_also_reports_an_empty_rotation_secret() {
        let verifier = Verifier::try_new(Secret::new("secret")).unwrap();

        assert_eq!(
            verifier.try_also(Secret::new("")).unwrap_err(),
            SecretError::Empty
        );
    }

    #[test]
    fn try_also_accepts_a_further_secret_as_also_does() {
        let verifier = Verifier::try_new(Secret::new("wrong"))
            .unwrap()
            .try_also(Secret::new("It's a Secret to Everybody"))
            .unwrap();

        assert_eq!(
            verifier.verify(DOCUMENTED_SIGNATURE, b"Hello, World!"),
            Ok(())
        );
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

        assert_eq!(verifier.verify(EMPTY_BODY_SIGNATURE, b""), Ok(()));
    }

    #[test]
    fn signs_githubs_documented_test_vector() {
        let verifier = Verifier::new(Secret::new("It's a Secret to Everybody"));

        assert_eq!(verifier.sign(b"Hello, World!"), DOCUMENTED_SIGNATURE);
    }

    #[test]
    fn signs_the_empty_body() {
        let verifier = Verifier::new(Secret::new("secret"));

        assert_eq!(verifier.sign(b""), EMPTY_BODY_SIGNATURE);
    }

    #[test]
    fn what_it_signs_it_verifies() {
        let verifier = Verifier::new(Secret::new("a secret nobody documented"));
        let body = "{\"action\":\"opened\",\"title\":\"caf\u{e9} \u{1F680}\"}".as_bytes();

        let signature = verifier.sign(body);

        assert_eq!(verifier.verify(&signature, body), Ok(()));
        assert_eq!(
            verifier.verify(&signature, b"{}"),
            Err(VerifyError::Mismatch),
            "the signature is over the body, not a constant"
        );
    }

    #[test]
    fn a_rotated_verifier_signs_under_its_first_secret() {
        let current_first = Verifier::new(Secret::new("It's a Secret to Everybody"))
            .also(Secret::new("previous secret"));
        let previous_first = Verifier::new(Secret::new("previous secret"))
            .also(Secret::new("It's a Secret to Everybody"));

        assert_eq!(current_first.sign(b"Hello, World!"), DOCUMENTED_SIGNATURE);

        let under_previous = previous_first.sign(b"Hello, World!");
        assert_ne!(under_previous, DOCUMENTED_SIGNATURE);
        assert_eq!(
            Verifier::new(Secret::new("previous secret")).verify(&under_previous, b"Hello, World!"),
            Ok(()),
            "the first secret signed it, so a verifier over that secret alone accepts it"
        );
        assert_eq!(
            previous_first.verify(&under_previous, b"Hello, World!"),
            Ok(()),
            "and so does the rotated verifier itself"
        );
    }

    #[test]
    fn debug_output_redacts_the_secrets() {
        let verifier = Verifier::new(Secret::new("super-secret"));

        let debug = format!("{verifier:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret"));
    }
}
