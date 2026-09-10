//! The `X-Hub-Signature-256` signature: the secret it is computed under, the
//! parsed value itself, the verifier that checks (and, for a test, produces)
//! it, and the failures a signature can have. The secret's bytes are read in
//! one method, the secret's own `sign`, and leave it only as an HMAC.

use std::{fmt, str::FromStr, sync::Arc};

use hmac::{Hmac, KeyInit, Mac};
use http::HeaderValue;
use sha2::Sha256;
use subtle::{Choice, ConstantTimeEq};
use thiserror::Error;
use zeroize::Zeroizing;

const SHA256_PREFIX: &str = "sha256=";
const SHA256_BYTES: usize = 32;
const SHA256_HEX_CHARS: usize = SHA256_BYTES * 2;

/// Bytes that cannot be a [`WebhookSecret`].
///
/// Reported by the fallible constructors of a [`WebhookSecret`],
/// [`str::parse`] for a string and `TryFrom<Vec<u8>>` or `TryFrom<&[u8]>`
/// for bytes; the panicking [`WebhookSecret::new`] panics with the same
/// message instead. This is a
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
/// accept any sender who guessed the key. Every constructor refuses one,
/// [`WebhookSecret::new`] by panicking and [`str::parse`], `TryFrom<Vec<u8>>`
/// and `TryFrom<&[u8]>` with a [`WebhookSecretError`], so a [`Verifier`]
/// cannot be handed one and has nothing left to check.
///
/// `Display` is deliberately not implemented: interpolating a secret into a
/// format string is a compile error rather than silently redacted output.
/// `Debug` output is redacted.
///
/// Each clone owns and independently zeroizes its own copy. The HMAC
/// implementation necessarily keeps derived key material outside this value;
/// that internal state is not guaranteed to be zeroized by the `hmac` crate.
#[derive(Clone)]
pub struct WebhookSecret(Zeroizing<Vec<u8>>);

impl WebhookSecret {
    /// Creates a secret from raw bytes.
    ///
    /// For a deployment that reads its secret at startup, where an empty one
    /// should stop the process. [`str::parse`], `TryFrom<Vec<u8>>` and
    /// `TryFrom<&[u8]>` report the same failure as a value, for one that
    /// reads it per request.
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

    /// The HMAC-SHA256 of `body` under this secret: the signature GitHub
    /// would send for the body if this were the webhook's secret.
    ///
    /// The one place the key bytes are read, and they leave it only as the
    /// HMAC: the verifier compares what this produces with what arrived, and
    /// never sees the key.
    fn sign(&self, body: &[u8]) -> Signature {
        // HMAC takes a key of any length: a long one is hashed to fit and a
        // short one is padded, so the key is never invalid.
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC accepts a key of any length");
        mac.update(body);
        Signature(mac.finalize().into_bytes().into())
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

/// Reads a secret from owned bytes, refusing empty ones as a value.
///
/// The fallible counterpart of [`WebhookSecret::new`] for a secret that is
/// not a string: one read from a file or a secret manager as bytes. The
/// vector is taken as it is, no copy, and zeroed when the secret drops.
///
/// ```
/// use octoevents::{WebhookSecret, WebhookSecretError};
///
/// let secret = WebhookSecret::try_from(b"current secret".to_vec())?;
/// # let _ = secret;
///
/// assert_eq!(WebhookSecret::try_from(Vec::new()).unwrap_err(), WebhookSecretError::Empty);
/// # Ok::<(), WebhookSecretError>(())
/// ```
impl TryFrom<Vec<u8>> for WebhookSecret {
    type Error = WebhookSecretError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Self::try_new(bytes)
    }
}

/// Reads a secret from borrowed bytes, copying them; see `TryFrom<Vec<u8>>`.
impl TryFrom<&[u8]> for WebhookSecret {
    type Error = WebhookSecretError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::try_new(bytes.to_vec())
    }
}

impl fmt::Debug for WebhookSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebhookSecret([REDACTED])")
    }
}

/// A failure to authenticate a webhook body by its signature.
///
/// Each variant is decided in one place, in the order a delivery meets them.
/// [`Missing`](Self::Missing) is decided from the headers, before anything is
/// parsed; [`Malformed`](Self::Malformed) by parsing the header into a
/// [`Signature`]; [`Mismatch`](Self::Mismatch) by [`Verifier::verify`], which
/// takes the parsed signature and so has no format left to refuse. Kept
/// apart from [`WebhookSecretError`], a configuration failure found before
/// any delivery arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum SignatureError {
    /// The `X-Hub-Signature-256` header was absent.
    ///
    /// Decided from the headers alone: the receiver reports it before the
    /// body is read, [`Envelope::from_signed`] before it reads anything else,
    /// and [`Verifier::verify`], which is handed a [`Signature`], never sees
    /// an absent one.
    ///
    /// [`Envelope::from_signed`]: crate::Envelope::from_signed
    #[error("missing X-Hub-Signature-256 header")]
    Missing,
    /// The header was present but not `sha256=` followed by exactly 64
    /// hexadecimal digits.
    ///
    /// Decided by parsing the header into a [`Signature`], through
    /// `TryFrom<&HeaderValue>`, `TryFrom<&[u8]>` or [`str::parse`], and
    /// nowhere else: bytes that are not visible ASCII, another algorithm's
    /// prefix, an uppercase prefix and a wrong length are all this variant.
    /// [`Verifier::verify`] cannot produce it, since a [`Signature`] has
    /// already parsed.
    #[error("malformed X-Hub-Signature-256 header")]
    Malformed,
    /// None of the configured secrets produced the signature for the body.
    ///
    /// Decided by [`Verifier::verify`], the one failure it has.
    #[error("webhook signature mismatch")]
    Mismatch,
}

/// A parsed `X-Hub-Signature-256` value: the 32 MAC bytes.
///
/// The one form a signature takes once it has left the wire. A header value
/// becomes one through `TryFrom<&http::HeaderValue>`, the header as a
/// `HeaderMap` holds it, through `TryFrom<&[u8]>` for a transport that has
/// the header's bytes in another shape, or through [`str::parse`] for a
/// string, and each refuses anything that is not `sha256=` followed by 64
/// hexadecimal digits as [`SignatureError::Malformed`]; the hex digits may
/// be upper- or lowercase, the prefix lowercase only, as GitHub sends it. A
/// `HeaderValue` is parsed from its bytes, since it need not be a string, so
/// one that is not visible ASCII is malformed like any other. [`Verifier::sign`]
/// produces one, and [`Verifier::verify`] takes one, so what reaches the
/// verifier has a settled format and the verifier's one failure is a
/// mismatch.
///
/// `Display` renders the header value back, `sha256=` and lowercase hex, and
/// `From<Signature> for HeaderValue` is the same text as the header a test
/// puts on its synthetic request; both are the inverse of parsing. `Debug`
/// is redacted: the value is secret-derived, and the crate records nothing
/// computed from the secret. There is no comparison, neither `PartialEq` nor
/// `ConstantTimeEq`, so that [`Verifier::verify`], in constant time over
/// every configured secret, is the one verification path the crate offers;
/// `signature == verifier.sign(body)` would check one secret and skip the
/// verify span.
///
/// ```
/// use http::HeaderValue;
/// use octoevents::{Signature, SignatureError};
///
/// let signature: Signature =
///     "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17".parse()?;
/// assert_eq!(
///     signature.to_string(),
///     "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
/// );
/// assert_eq!(format!("{signature:?}"), "Signature([REDACTED])");
///
/// let header = HeaderValue::from(signature);
/// assert_eq!(header, "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17");
/// assert!(Signature::try_from(&header).is_ok());
///
/// assert_eq!(
///     "sha1=757107ea0eb2509fc211221cce984b8a37570b6d".parse::<Signature>().unwrap_err(),
///     SignatureError::Malformed
/// );
/// # Ok::<(), SignatureError>(())
/// ```
///
/// Neither `==` nor `ct_eq` compiles on two signatures, the trait in scope
/// or not; the check is [`Verifier::verify`]:
///
/// ```compile_fail,E0369
/// use octoevents::{Verifier, WebhookSecret};
///
/// let verifier = Verifier::new(WebhookSecret::new("secret"));
/// let _ = verifier.sign(b"a") == verifier.sign(b"a");
/// ```
///
/// ```compile_fail,E0599
/// use octoevents::{Verifier, WebhookSecret};
/// use subtle::ConstantTimeEq;
///
/// let verifier = Verifier::new(WebhookSecret::new("secret"));
/// let _ = verifier.sign(b"a").ct_eq(&verifier.sign(b"a"));
/// ```
#[derive(Clone)]
pub struct Signature([u8; SHA256_BYTES]);

/// Parses the header's bytes, for a transport that has them and no string.
///
/// # Errors
///
/// Returns [`SignatureError::Malformed`] for anything that is not `sha256=`
/// followed by 64 hexadecimal digits, bytes that are not ASCII included.
impl TryFrom<&[u8]> for Signature {
    type Error = SignatureError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        parse_signature(value)
            .map(Self)
            .ok_or(SignatureError::Malformed)
    }
}

/// Parses the header value as an `http::HeaderMap` holds it, from its bytes:
/// a `HeaderValue` need not be visible ASCII, and one that is not is a
/// present header that is not a signature.
///
/// # Errors
///
/// Returns [`SignatureError::Malformed`] for anything that is not `sha256=`
/// followed by 64 hexadecimal digits.
impl TryFrom<&HeaderValue> for Signature {
    type Error = SignatureError;

    fn try_from(value: &HeaderValue) -> Result<Self, Self::Error> {
        Self::try_from(value.as_bytes())
    }
}

/// Parses the header value.
///
/// # Errors
///
/// Returns [`SignatureError::Malformed`] for anything that is not `sha256=`
/// followed by 64 hexadecimal digits.
impl FromStr for Signature {
    type Err = SignatureError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.as_bytes())
    }
}

/// The header value: `sha256=` followed by the MAC as lowercase hex.
impl fmt::Display for Signature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(SHA256_PREFIX)?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The header value as an `http::HeaderValue`, the text `Display` renders:
/// what `.header(header::SIGNATURE, signature)` puts on a request. Marked
/// sensitive, so `http`'s own `Debug` redacts it as this type's does.
impl From<Signature> for HeaderValue {
    fn from(signature: Signature) -> Self {
        let mut value = Self::try_from(signature.to_string())
            .expect("`sha256=` and 64 hex digits are visible ASCII, which is a header value");
        value.set_sensitive(true);
        value
    }
}

/// Redacted: the MAC is secret-derived.
impl fmt::Debug for Signature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Signature([REDACTED])")
    }
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

    /// Verifies a parsed `X-Hub-Signature-256` value over the exact body
    /// bytes.
    ///
    /// Every secret is evaluated even after a match, so timing reveals
    /// neither the secret nor which one matched. This provides
    /// authentication but not replay protection; deduplicate downstream using
    /// `X-GitHub-Delivery`.
    ///
    /// The signature arrives parsed. A header value becomes a [`Signature`]
    /// through `TryFrom<&HeaderValue>`, the conversion the receiving path
    /// runs on the header as an `http::HeaderMap` holds it, through
    /// `TryFrom<&[u8]>` for the bytes in another shape, or through
    /// [`str::parse`] for a string, and each is where a value that is not
    /// `sha256=` and 64 hexadecimal digits is refused as
    /// [`SignatureError::Malformed`]. By the time a signature reaches this
    /// method its format is settled, so the only failure left is that no
    /// secret produced it.
    ///
    /// ```
    /// use octoevents::{Signature, Verifier, WebhookSecret};
    ///
    /// let verifier = Verifier::new(WebhookSecret::new("It's a Secret to Everybody"));
    /// let signature: Signature =
    ///     "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17".parse()?;
    /// verifier.verify(&signature, b"Hello, World!")?;
    /// # Ok::<(), octoevents::SignatureError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SignatureError::Mismatch`] when no configured secret
    /// produced the signature for `body`. Nothing else: the format was
    /// checked when the signature was parsed.
    pub fn verify(&self, signature: &Signature, body: &[u8]) -> Result<(), SignatureError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            "octoevents.verify",
            secret_count = self.secrets.len(),
            body_len = body.len(),
            outcome = tracing::field::Empty,
        );
        #[cfg(feature = "tracing")]
        let _entered = span.enter();
        let mut matched = Choice::from(0);
        for secret in self.secrets.iter() {
            matched |= secret.sign(body).0.ct_eq(&signature.0);
        }

        if bool::from(matched) {
            #[cfg(feature = "tracing")]
            span.record("outcome", "verified");
            Ok(())
        } else {
            #[cfg(feature = "tracing")]
            span.record("outcome", "mismatch");
            Err(SignatureError::Mismatch)
        }
    }

    /// The signature GitHub would send for `body`: its HMAC-SHA256 under the
    /// first configured secret, as a [`Signature`], which converts into the
    /// `X-Hub-Signature-256` header value with `HeaderValue::from` and
    /// renders it with `Display`, `sha256=` followed by lowercase hex.
    ///
    /// A test aid for the receiving side. A test drives the receiver with a
    /// synthetic request, and that request needs the signature GitHub would
    /// have put on it, so the test signs the body with the verifier the
    /// receiver was built with and puts the result on the header:
    /// `.header(header::SIGNATURE, verifier.sign(body))`. The crate sends
    /// nothing; the method exists so the test needs no HMAC code of its own.
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
    /// assert_eq!(
    ///     signature.to_string(),
    ///     "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
    /// );
    /// verifier.verify(&signature, b"Hello, World!")?;
    /// # Ok::<(), octoevents::SignatureError>(())
    /// ```
    #[must_use]
    pub fn sign(&self, body: &[u8]) -> Signature {
        // `new` puts one secret in and nothing takes one out.
        self.secrets[0].sign(body)
    }
}

/// Accepts further secrets, [`Verifier::also`] over an iterator, for a
/// rotation window read from configuration as a list.
///
/// ```
/// use octoevents::{Verifier, WebhookSecret};
///
/// let previous = ["previous secret", "older secret"];
///
/// let mut verifier = Verifier::new(WebhookSecret::new("current secret"));
/// verifier.extend(previous.map(WebhookSecret::new));
/// # let _ = verifier;
/// ```
impl Extend<WebhookSecret> for Verifier {
    fn extend<T: IntoIterator<Item = WebhookSecret>>(&mut self, secrets: T) {
        Arc::make_mut(&mut self.secrets).extend(secrets);
    }
}

/// The MAC bytes of a `sha256=<64 hex digits>` header value, or `None` for
/// anything else: another prefix, a wrong length, a byte that is not a hex
/// digit, a byte that is not ASCII at all. The one decision behind
/// [`SignatureError::Malformed`]; the inverse of `Signature`'s `Display`.
fn parse_signature(value: &[u8]) -> Option<[u8; SHA256_BYTES]> {
    let hex = value.strip_prefix(SHA256_PREFIX.as_bytes())?;
    if hex.len() != SHA256_HEX_CHARS {
        return None;
    }

    // The length check above guarantees an exact number of pairs and no remainder.
    let (pairs, _) = hex.as_chunks::<2>();
    let mut decoded = [0_u8; SHA256_BYTES];
    for (output, &[high, low]) in decoded.iter_mut().zip(pairs) {
        *output = (hex_nibble(high)? << 4) | hex_nibble(low)?;
    }
    Some(decoded)
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
