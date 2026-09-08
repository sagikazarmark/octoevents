//! The `X-Hub-Signature-256` signature: the secret it is computed under, the
//! verifier that checks (and, for a test, produces) it, and the failures a
//! signature can have. The secret's bytes leave this module only as an HMAC.

use std::{fmt, str::FromStr, sync::Arc};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::trace;

const SHA256_PREFIX: &str = "sha256=";
const SHA256_BYTES: usize = 32;
const SHA256_HEX_CHARS: usize = SHA256_BYTES * 2;

/// Bytes that cannot be a [`WebhookSecret`].
///
/// Reported by [`str::parse`] into a [`WebhookSecret`]; the panicking
/// [`WebhookSecret::new`] panics with the same message instead. This is a
/// configuration failure, found before any delivery arrives, and so is kept
/// apart from [`SignatureError`], which reports a body that did not
/// authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum WebhookSecretError {
    /// The secret has no bytes.
    ///
    /// An empty secret is the unset- or mistyped-environment-variable failure
    /// mode, not a configuration: every delivery would verify against a
    /// guessable key.
    #[error("webhook secret must not be empty")]
    Empty,
}

/// A webhook secret whose owned bytes are zeroed when dropped.
///
/// A secret is never empty: an empty one is the unset- or
/// mistyped-environment-variable failure mode, and verifying against it would
/// accept any sender who guessed the key. Both constructors refuse one, so a
/// [`Verifier`] cannot be handed one and has nothing left to check.
///
/// `Display` is deliberately not implemented: interpolating a secret into a
/// format string is a compile error rather than silently redacted output.
/// `Debug` output is redacted.
///
/// Each clone owns and independently zeroizes its own copy. The HMAC
/// implementation necessarily keeps derived key material outside this value;
/// that internal state is not guaranteed to be zeroized by the `hmac` crate.
pub struct WebhookSecret(Zeroizing<Vec<u8>>);

impl WebhookSecret {
    /// Creates a secret from raw bytes.
    ///
    /// For a deployment that reads its secret at startup, where an empty one
    /// should stop the process. [`str::parse`] reports the same failure as a
    /// value, for one that reads it per request.
    ///
    /// # Panics
    ///
    /// Panics when `bytes` is empty, with [`WebhookSecretError::Empty`]'s
    /// message.
    #[must_use]
    #[track_caller]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        match Self::try_new(bytes.into()) {
            Ok(secret) => secret,
            Err(error) => panic!("{error}"),
        }
    }

    fn try_new(bytes: Vec<u8>) -> Result<Self, WebhookSecretError> {
        if bytes.is_empty() {
            return Err(WebhookSecretError::Empty);
        }
        Ok(Self(Zeroizing::new(bytes)))
    }

    /// The key bytes, for [`hmac_sha256`] alone.
    fn expose(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl Clone for WebhookSecret {
    fn clone(&self) -> Self {
        // The bytes were checked when `self` was made.
        Self(Zeroizing::new(self.0.to_vec()))
    }
}

/// Reads a secret from a string, refusing an empty one as a value.
///
/// The fallible counterpart of [`WebhookSecret::new`], for a deployment that
/// builds its verifier where a panic is the wrong answer: a serverless
/// function reading its secret per request, say, where an unset variable
/// should be a response and not a trap.
///
/// ```
/// use octoevents::{WebhookSecret, WebhookSecretError};
///
/// let secret: WebhookSecret = "current secret".parse()?;
/// # let _ = secret;
///
/// assert_eq!("".parse::<WebhookSecret>().unwrap_err(), WebhookSecretError::Empty);
/// # Ok::<(), WebhookSecretError>(())
/// ```
impl FromStr for WebhookSecret {
    type Err = WebhookSecretError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value.as_bytes().to_vec())
    }
}

impl fmt::Debug for WebhookSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebhookSecret([REDACTED])")
    }
}

/// A failure to authenticate a webhook body by its signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum SignatureError {
    /// The `X-Hub-Signature-256` header was absent.
    ///
    /// Envelope constructors produce this variant while extracting headers;
    /// [`Verifier::verify`] itself accepts an already-extracted header value.
    #[error("missing X-Hub-Signature-256 header")]
    Missing,
    /// The signature was not `sha256=` followed by exactly 64 hexadecimal characters.
    #[error("malformed X-Hub-Signature-256 header")]
    Malformed,
    /// None of the configured secrets matched the signature.
    #[error("webhook signature mismatch")]
    Mismatch,
}

/// The configured secrets, and the HMAC comparison they authenticate with.
///
/// A verifier is required to receive a webhook: it is constructed from one
/// secret, so a deployment without a secret cannot be expressed, and a
/// [`WebhookSecret`] is never empty, so neither can one with a guessable key.
/// Additional secrets are added with [`Verifier::also`] to open a client-side
/// rotation window.
///
/// Clones share the secrets rather than copying them: a receiver holds its
/// verifier and borrows it per delivery, so the clone is for a verifier that
/// is handed to more than one receiver, or kept beside the one receiving so
/// a test can sign with it, without secret material being copied onto the
/// heap each time.
///
/// Only `X-Hub-Signature-256` is verified. GitHub sends the SHA-1
/// `X-Hub-Signature` beside it, and a request carrying only that header is
/// [`SignatureError::Missing`]: the SHA-256 header is always there to
/// verify, so falling back to the SHA-1 one would protect no delivery and
/// would let a sender choose the weaker algorithm.
///
/// ```
/// use octoevents::{Verifier, WebhookSecret};
///
/// let verifier = Verifier::new(WebhookSecret::new("current secret"))
///     .also(WebhookSecret::new("previous secret"));
/// # let _ = verifier;
/// ```
#[derive(Debug, Clone)]
pub struct Verifier {
    secrets: Arc<Vec<WebhookSecret>>,
}

impl Verifier {
    /// Creates a verifier authenticating against one secret.
    #[must_use]
    pub fn new(secret: WebhookSecret) -> Self {
        Self {
            secrets: Arc::new(vec![secret]),
        }
    }

    /// Accepts a further secret, for client-side credential rotation.
    ///
    /// GitHub configures exactly one secret per webhook, so a rotation window
    /// lives here: accept the new secret alongside the old one, change it in
    /// the App settings, then drop the old one once in-flight deliveries drain.
    /// Every secret is tried against a well-formed signature, so the order
    /// matters only to [`Verifier::sign`], which signs under the first, the
    /// one [`Verifier::new`] received.
    #[must_use]
    pub fn also(mut self, secret: WebhookSecret) -> Self {
        Arc::make_mut(&mut self.secrets).push(secret);
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
    /// uppercase prefix is [`SignatureError::Malformed`], not a mismatch.
    ///
    /// ```
    /// use octoevents::{Verifier, WebhookSecret};
    ///
    /// let verifier = Verifier::new(WebhookSecret::new("It's a Secret to Everybody"));
    /// let signature = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
    /// verifier.verify(signature, b"Hello, World!")?;
    /// # Ok::<(), octoevents::SignatureError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SignatureError::Malformed`] for any value not matching
    /// GitHub's `sha256=<64 hex characters>` format and
    /// [`SignatureError::Mismatch`] when no secret matches.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            name = "octoevents.verify",
            level = "debug",
            skip_all,
            fields(secret_count = self.secrets.len(), body_len = body.len(), outcome = tracing::field::Empty)
        )
    )]
    pub fn verify(&self, signature_header: &str, body: &[u8]) -> Result<(), SignatureError> {
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
            Err(SignatureError::Mismatch)
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
    /// use octoevents::{Verifier, WebhookSecret};
    ///
    /// let verifier = Verifier::new(WebhookSecret::new("It's a Secret to Everybody"));
    /// let signature = verifier.sign(b"Hello, World!");
    ///
    /// assert_eq!(signature, "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17");
    /// verifier.verify(&signature, b"Hello, World!")?;
    /// # Ok::<(), octoevents::SignatureError>(())
    /// ```
    #[must_use]
    pub fn sign(&self, body: &[u8]) -> String {
        // `new` puts one secret in and nothing takes one out.
        let tag = hmac_sha256(&self.secrets[0], body);
        encode_signature(&tag)
    }
}

/// The HMAC-SHA256 of `body` under `secret`.
fn hmac_sha256(secret: &WebhookSecret, body: &[u8]) -> [u8; SHA256_BYTES] {
    // HMAC takes a key of any length: a long one is hashed to fit and a short
    // one is padded, so the key is never invalid.
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.expose()).expect("HMAC accepts a key of any length");
    mac.update(body);
    mac.finalize().into_bytes().into()
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

fn decode_signature(value: &str) -> Result<[u8; SHA256_BYTES], SignatureError> {
    let hex = value
        .strip_prefix(SHA256_PREFIX)
        .ok_or(SignatureError::Malformed)?;
    if hex.len() != SHA256_HEX_CHARS {
        return Err(SignatureError::Malformed);
    }

    // The length check above guarantees an exact number of pairs and no remainder.
    let (pairs, _) = hex.as_bytes().as_chunks::<2>();
    let mut decoded = [0_u8; SHA256_BYTES];
    for (output, &[high, low]) in decoded.iter_mut().zip(pairs) {
        let high = hex_nibble(high).ok_or(SignatureError::Malformed)?;
        let low = hex_nibble(low).ok_or(SignatureError::Malformed)?;
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
mod tests;
