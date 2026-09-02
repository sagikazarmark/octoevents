use crate::{ReceiveError, VerifyError};

/// The transport-independent status selected for a receive outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResponseStatus {
    /// The delivery was accepted or a ping was short-circuited.
    NoContent,
    /// Request metadata or body framing was malformed.
    BadRequest,
    /// Authentication was absent or did not match.
    Unauthorized,
    /// The request exceeded the configured body limit.
    PayloadTooLarge,
    /// The consumer handler failed.
    InternalServerError,
}

impl ResponseStatus {
    /// Returns the HTTP status code.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        match self {
            Self::NoContent => 204,
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::PayloadTooLarge => 413,
            Self::InternalServerError => 500,
        }
    }

    /// Selects the status for a receive failure, per the crate's response contract.
    ///
    /// `WebhookReceiver` applies this mapping itself; it is public so a
    /// transport built directly on [`Envelope::from_signed`] can answer
    /// GitHub the same way.
    ///
    /// [`Envelope::from_signed`]: crate::Envelope::from_signed
    #[must_use]
    pub fn for_receive_error(error: &ReceiveError) -> Self {
        match error {
            ReceiveError::Verify(VerifyError::MissingSignature | VerifyError::Mismatch) => {
                Self::Unauthorized
            }
            ReceiveError::Verify(VerifyError::MalformedSignature)
            | ReceiveError::MissingHeader(_)
            | ReceiveError::UnsupportedContentType => Self::BadRequest,
            ReceiveError::BodyTooLarge { .. } => Self::PayloadTooLarge,
        }
    }
}

#[cfg(feature = "http")]
#[cfg_attr(docsrs, doc(cfg(feature = "http")))]
impl From<ResponseStatus> for http::StatusCode {
    fn from(status: ResponseStatus) -> Self {
        match status {
            ResponseStatus::NoContent => Self::NO_CONTENT,
            ResponseStatus::BadRequest => Self::BAD_REQUEST,
            ResponseStatus::Unauthorized => Self::UNAUTHORIZED,
            ResponseStatus::PayloadTooLarge => Self::PAYLOAD_TOO_LARGE,
            ResponseStatus::InternalServerError => Self::INTERNAL_SERVER_ERROR,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ReceiveError, ResponseStatus, VerifyError};

    #[test]
    fn maps_the_receive_contract() {
        assert_eq!(
            ResponseStatus::for_receive_error(&ReceiveError::Verify(VerifyError::MissingSignature)),
            ResponseStatus::Unauthorized
        );
        assert_eq!(
            ResponseStatus::for_receive_error(&ReceiveError::Verify(
                VerifyError::MalformedSignature
            )),
            ResponseStatus::BadRequest
        );
        assert_eq!(
            ResponseStatus::for_receive_error(&ReceiveError::BodyTooLarge { limit: 1 }),
            ResponseStatus::PayloadTooLarge
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn converts_to_the_matching_http_status_code() {
        for status in [
            ResponseStatus::NoContent,
            ResponseStatus::BadRequest,
            ResponseStatus::Unauthorized,
            ResponseStatus::PayloadTooLarge,
            ResponseStatus::InternalServerError,
        ] {
            assert_eq!(http::StatusCode::from(status).as_u16(), status.as_u16());
        }
    }
}
