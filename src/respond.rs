use crate::{ReceiveError, VerifyError};

/// The transport-independent status selected for a receive outcome.
///
/// [`as_u16`](Self::as_u16) is the code for a transport with its own status
/// type; with the `http` feature it also converts into `http::StatusCode`,
/// an impl that lives with the receiver.
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

#[cfg(test)]
mod tests {
    use crate::{ReceiveError, ResponseStatus, VerifyError, header};

    #[test]
    fn maps_every_receive_error_to_the_status_the_contract_names() {
        // The whole table: an absent or mismatched signature is the client's
        // authentication failing (401); a signature that is not `sha256=` and
        // 64 hex characters, a missing required header and a form-encoded
        // body are malformed requests (400); the body limit is its own code
        // (413). One row per `ReceiveError` shape the match above has an arm
        // for. Which failure a request earns is `Envelope::from_signed`'s
        // test; that the receiver answers with the mapped status is its own.
        let table = [
            (
                ReceiveError::Verify(VerifyError::MissingSignature),
                ResponseStatus::Unauthorized,
            ),
            (
                ReceiveError::Verify(VerifyError::Mismatch),
                ResponseStatus::Unauthorized,
            ),
            (
                ReceiveError::Verify(VerifyError::MalformedSignature),
                ResponseStatus::BadRequest,
            ),
            (
                ReceiveError::MissingHeader(header::DELIVERY_ID),
                ResponseStatus::BadRequest,
            ),
            (
                ReceiveError::UnsupportedContentType,
                ResponseStatus::BadRequest,
            ),
            (
                ReceiveError::BodyTooLarge { limit: 1 },
                ResponseStatus::PayloadTooLarge,
            ),
        ];

        for (error, status) in table {
            assert_eq!(
                ResponseStatus::for_receive_error(&error),
                status,
                "{error:?}"
            );
        }
    }
}
