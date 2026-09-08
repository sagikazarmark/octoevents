use std::{fmt, str::FromStr};

use thiserror::Error;
use zeroize::Zeroizing;

/// Bytes that cannot be a [`Secret`].
///
/// Reported by [`str::parse`] into a [`Secret`]; the panicking
/// [`Secret::new`] panics with the same message instead. This is a
/// configuration failure, found before any delivery arrives, and so is kept
/// apart from [`VerifyError`](crate::VerifyError), which reports a body that
/// did not authenticate.
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

/// A webhook secret whose owned bytes are zeroed when dropped.
///
/// A secret is never empty: an empty one is the unset- or
/// mistyped-environment-variable failure mode, and verifying against it would
/// accept any sender who guessed the key. Both constructors refuse one, so a
/// [`Verifier`](crate::Verifier) cannot be handed one and has nothing left to
/// check.
///
/// `Display` is deliberately not implemented: interpolating a secret into a
/// format string is a compile error rather than silently redacted output.
/// `Debug` output is redacted.
///
/// Each clone owns and independently zeroizes its own copy. The HMAC
/// implementation necessarily keeps derived key material outside this value;
/// that internal state is not guaranteed to be zeroized by the `hmac` crate.
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    /// Creates a secret from raw bytes.
    ///
    /// For a deployment that reads its secret at startup, where an empty one
    /// should stop the process. [`str::parse`] reports the same failure as a
    /// value, for one that reads it per request.
    ///
    /// # Panics
    ///
    /// Panics when `bytes` is empty, with [`SecretError::Empty`]'s message.
    #[must_use]
    #[track_caller]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        match Self::try_new(bytes.into()) {
            Ok(secret) => secret,
            Err(error) => panic!("{error}"),
        }
    }

    fn try_new(bytes: Vec<u8>) -> Result<Self, SecretError> {
        if bytes.is_empty() {
            return Err(SecretError::Empty);
        }
        Ok(Self(Zeroizing::new(bytes)))
    }

    pub(crate) fn expose(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        // The bytes were checked when `self` was made.
        Self(Zeroizing::new(self.0.to_vec()))
    }
}

/// Reads a secret from a string, refusing an empty one as a value.
///
/// The fallible counterpart of [`Secret::new`], for a deployment that builds
/// its verifier where a panic is the wrong answer: a serverless function
/// reading its secret per request, say, where an unset variable should be a
/// response and not a trap.
///
/// ```
/// use octoevents::{Secret, SecretError};
///
/// let secret: Secret = "current secret".parse()?;
/// # let _ = secret;
///
/// assert_eq!("".parse::<Secret>().unwrap_err(), SecretError::Empty);
/// # Ok::<(), SecretError>(())
/// ```
impl FromStr for Secret {
    type Err = SecretError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value.as_bytes().to_vec())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::{Secret, SecretError};

    #[test]
    fn debug_formatting_is_redacted() {
        let secret = Secret::new("super-secret");

        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
    }

    #[test]
    #[should_panic(expected = "webhook secret must not be empty")]
    fn new_refuses_an_empty_secret() {
        let _ = Secret::new("");
    }

    #[test]
    fn parse_reports_an_empty_secret_instead_of_panicking() {
        // The environment-variable failure mode as a value: a deployment that
        // reads its secret at request time answers instead of trapping.
        let error = "".parse::<Secret>().unwrap_err();

        assert_eq!(error, SecretError::Empty);
        assert_eq!(error.to_string(), "webhook secret must not be empty");
    }

    #[test]
    fn parse_builds_the_secret_new_would() {
        let parsed: Secret = "super-secret".parse().unwrap();

        assert_eq!(parsed.expose(), Secret::new("super-secret").expose());
    }

    #[test]
    fn a_clone_carries_the_same_bytes() {
        let secret = Secret::new("super-secret");

        assert_eq!(secret.clone().expose(), secret.expose());
    }
}
