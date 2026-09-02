use std::{fmt, str::FromStr};

use zeroize::Zeroizing;

/// A webhook secret whose owned bytes are zeroed when dropped.
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
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(Zeroizing::new(bytes.into()))
    }

    pub(crate) fn expose(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self::new(self.0.to_vec())
    }
}

impl FromStr for Secret {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(value.as_bytes()))
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn debug_formatting_is_redacted() {
        let secret = Secret::new("super-secret");

        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
    }
}
