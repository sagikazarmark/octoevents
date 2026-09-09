//! The signature module's tests, beside the production code so they reach
//! the verifier's secrets and the secret's bytes. Grouped by concern: the
//! secret as a value, the signature as a value, then verifying and signing.

use std::sync::Arc;

use super::{Signature, SignatureError, Verifier, WebhookSecret, WebhookSecretError};

/// GitHub's documented test vector: `Hello, World!` under `It's a
/// Secret to Everybody`.
const DOCUMENTED_SIGNATURE: &str =
    "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
/// The empty body under `secret`.
const EMPTY_BODY_SIGNATURE: &str =
    "sha256=f9e66e179b6747ae54108f82f8ade8b3c25d76fd30afde6c395822c530196169";

/// `value` as the parsed signature it is known to be.
fn parsed(value: &str) -> Signature {
    value.parse().unwrap()
}

mod secret {
    use super::{WebhookSecret, WebhookSecretError};

    #[test]
    fn debug_formatting_is_redacted() {
        let secret = WebhookSecret::new("super-secret");

        assert_eq!(format!("{secret:?}"), "WebhookSecret([REDACTED])");
    }

    #[test]
    #[should_panic(expected = "webhook secret must not be empty")]
    fn new_refuses_an_empty_secret() {
        let _ = WebhookSecret::new("");
    }

    #[test]
    fn parse_reports_an_empty_secret_instead_of_panicking() {
        // The environment-variable failure mode as a value: a deployment that
        // reads its secret at request time answers instead of trapping.
        let error = "".parse::<WebhookSecret>().unwrap_err();

        assert_eq!(error, WebhookSecretError::Empty);
        assert_eq!(error.to_string(), "webhook secret must not be empty");
    }

    #[test]
    fn parse_builds_the_secret_new_would() {
        let parsed: WebhookSecret = "super-secret".parse().unwrap();

        assert_eq!(*parsed.0, *WebhookSecret::new("super-secret").0);
    }

    #[test]
    fn bytes_build_the_secret_new_would_and_refuse_empty_ones_as_a_value() {
        // The two byte shapes a secret read from a file or a secret manager
        // arrives in, each the fallible counterpart of `new` for bytes as
        // `parse` is for a string.
        let owned = WebhookSecret::try_from(b"super-secret".to_vec()).unwrap();
        let borrowed = WebhookSecret::try_from(&b"super-secret"[..]).unwrap();

        assert_eq!(*owned.0, *WebhookSecret::new("super-secret").0);
        assert_eq!(*borrowed.0, *owned.0);
        assert_eq!(
            WebhookSecret::try_from(Vec::new()).unwrap_err(),
            WebhookSecretError::Empty
        );
        assert_eq!(
            WebhookSecret::try_from(&[][..]).unwrap_err(),
            WebhookSecretError::Empty
        );
    }

    #[test]
    fn a_clone_carries_the_same_bytes() {
        let secret = WebhookSecret::new("super-secret");

        assert_eq!(*secret.clone().0, *secret.0);
    }
}

mod signature {
    use http::HeaderValue;
    use subtle::ConstantTimeEq as _;

    use super::{DOCUMENTED_SIGNATURE, EMPTY_BODY_SIGNATURE, Signature, SignatureError};

    #[test]
    fn parses_githubs_documented_test_vector_and_renders_it_back() {
        let signature: Signature = DOCUMENTED_SIGNATURE.parse().unwrap();

        assert_eq!(signature.to_string(), DOCUMENTED_SIGNATURE);
    }

    #[test]
    fn accepts_uppercase_hex_and_renders_lowercase() {
        // The prefix stays lowercase, as GitHub sends it; the digits after it
        // are read in either case and always written back in lowercase.
        let uppercase = DOCUMENTED_SIGNATURE
            .to_uppercase()
            .replacen("SHA256", "sha256", 1);

        let signature: Signature = uppercase.parse().unwrap();

        assert_eq!(signature.to_string(), DOCUMENTED_SIGNATURE);
    }

    #[test]
    fn rejects_malformed_values() {
        for value in [
            "",
            "sha1=757107ea0eb2509fc211221cce984b8a37570b6d",
            "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
            "sha256=757107ea",
            "sha256=z57107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
            // The prefix is deliberately case-sensitive: GitHub sends lowercase.
            "SHA256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
        ] {
            assert_eq!(
                value.parse::<Signature>().unwrap_err(),
                SignatureError::Malformed,
                "value: {value}"
            );
        }
    }

    #[test]
    fn parses_bytes_as_it_parses_a_string_and_refuses_bytes_that_are_not_ascii() {
        // The `http::HeaderValue` path hands over bytes, not a string: the
        // same bytes as a well-formed value parse as the string does, and a
        // header that is not visible ASCII is malformed, not missing.
        assert_eq!(
            Signature::try_from(DOCUMENTED_SIGNATURE.as_bytes())
                .unwrap()
                .to_string(),
            DOCUMENTED_SIGNATURE
        );
        assert_eq!(
            Signature::try_from(b"sha256=\xff\xfe".as_slice()).unwrap_err(),
            SignatureError::Malformed
        );
    }

    #[test]
    fn parses_a_header_value_from_its_bytes() {
        // The shape a `HeaderMap` holds the header in. A value that is not
        // visible ASCII, which `HeaderValue` admits, is malformed like any
        // other, so it is never mistaken for an absent header.
        let header = HeaderValue::from_static(DOCUMENTED_SIGNATURE);
        assert_eq!(
            Signature::try_from(&header).unwrap().to_string(),
            DOCUMENTED_SIGNATURE
        );

        let opaque = HeaderValue::from_bytes(b"sha256=\xff\xfe").unwrap();
        assert_eq!(
            Signature::try_from(&opaque).unwrap_err(),
            SignatureError::Malformed
        );
    }

    #[test]
    fn converts_into_the_header_value_it_renders_and_marks_it_sensitive() {
        // The inverse of parsing, as `Display` is, in the type a request
        // takes; sensitive so `http`'s `Debug` of a request redacts it as
        // this type's `Debug` does, and an HTTP/2 hop does not index it.
        let signature: Signature = DOCUMENTED_SIGNATURE.parse().unwrap();

        let header = HeaderValue::from(signature);

        assert_eq!(header, DOCUMENTED_SIGNATURE);
        assert!(header.is_sensitive());
        assert_eq!(format!("{header:?}"), "Sensitive");
        assert_eq!(
            Signature::try_from(&header).unwrap().to_string(),
            DOCUMENTED_SIGNATURE
        );
    }

    #[test]
    fn debug_formatting_is_redacted() {
        // A signature is secret-derived: `Display` is for the header a test
        // puts on its request, `Debug` for a log line, which must not carry
        // the MAC.
        let signature: Signature = DOCUMENTED_SIGNATURE.parse().unwrap();

        let debug = format!("{signature:?}");
        assert_eq!(debug, "Signature([REDACTED])");
        assert!(!debug.contains("757107ea"));
    }

    #[test]
    fn constant_time_equality_tells_signatures_apart() {
        // The comparison the verifier folds over its secrets: equal MACs are
        // equal, different ones are not. That it runs in constant time is
        // `subtle`'s promise, not something a test can time.
        let signature: Signature = DOCUMENTED_SIGNATURE.parse().unwrap();
        let same: Signature = DOCUMENTED_SIGNATURE.parse().unwrap();
        let other: Signature = EMPTY_BODY_SIGNATURE.parse().unwrap();

        assert!(bool::from(signature.ct_eq(&same)));
        assert!(!bool::from(signature.ct_eq(&other)));
    }
}

mod verify {
    use super::{
        Arc, DOCUMENTED_SIGNATURE, EMPTY_BODY_SIGNATURE, SignatureError, Verifier, WebhookSecret,
        parsed,
    };

    #[test]
    fn accepts_githubs_documented_test_vector() {
        let verifier = Verifier::new(WebhookSecret::new("It's a Secret to Everybody"));

        assert_eq!(
            verifier.verify(&parsed(DOCUMENTED_SIGNATURE), b"Hello, World!"),
            Ok(())
        );
    }

    #[test]
    fn tries_every_configured_secret() {
        let verifier = Verifier::new(WebhookSecret::new("wrong"))
            .also(WebhookSecret::new("It's a Secret to Everybody"));

        assert_eq!(
            verifier.verify(&parsed(DOCUMENTED_SIGNATURE), b"Hello, World!"),
            Ok(())
        );
    }

    #[test]
    fn clones_share_the_secrets_until_one_of_them_adds_another() {
        let original = Verifier::new(WebhookSecret::new("It's a Secret to Everybody"));
        let clone = original.clone();
        assert!(Arc::ptr_eq(&original.secrets, &clone.secrets));

        let extended = clone.also(WebhookSecret::new("wrong"));
        assert_eq!(original.secrets.len(), 1);
        assert_eq!(extended.secrets.len(), 2);
        assert_eq!(
            original.verify(&parsed(DOCUMENTED_SIGNATURE), b"Hello, World!"),
            Ok(())
        );
    }

    #[test]
    fn rejects_wrong_secrets() {
        let verifier =
            Verifier::new(WebhookSecret::new("wrong")).also(WebhookSecret::new("also wrong"));

        assert_eq!(
            verifier.verify(&parsed(DOCUMENTED_SIGNATURE), b"Hello, World!"),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn empty_body_is_still_authenticated() {
        let verifier = Verifier::new(WebhookSecret::new("secret"));

        assert_eq!(verifier.verify(&parsed(EMPTY_BODY_SIGNATURE), b""), Ok(()));
    }

    #[test]
    fn debug_output_redacts_the_secrets() {
        let verifier = Verifier::new(WebhookSecret::new("super-secret"));

        let debug = format!("{verifier:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret"));
    }
}

mod sign {
    use super::{
        DOCUMENTED_SIGNATURE, EMPTY_BODY_SIGNATURE, SignatureError, Verifier, WebhookSecret,
    };

    #[test]
    fn signs_githubs_documented_test_vector() {
        let verifier = Verifier::new(WebhookSecret::new("It's a Secret to Everybody"));

        assert_eq!(
            verifier.sign(b"Hello, World!").to_string(),
            DOCUMENTED_SIGNATURE
        );
    }

    #[test]
    fn a_verifier_from_a_secret_signs_under_it_and_extending_appends_after_it() {
        // `From` is `new`, and `Extend` is `also` over an iterator: the first
        // secret stays first, so `sign` still signs under it, and the
        // extended-in secrets verify beside it.
        let mut verifier = Verifier::from(WebhookSecret::new("It's a Secret to Everybody"));
        verifier.extend([WebhookSecret::new("previous"), WebhookSecret::new("older")]);

        assert_eq!(
            verifier.sign(b"Hello, World!").to_string(),
            DOCUMENTED_SIGNATURE
        );
        let under_older = Verifier::new(WebhookSecret::new("older")).sign(b"Hello, World!");
        assert_eq!(verifier.verify(&under_older, b"Hello, World!"), Ok(()));
    }

    #[test]
    fn signs_the_empty_body() {
        let verifier = Verifier::new(WebhookSecret::new("secret"));

        assert_eq!(verifier.sign(b"").to_string(), EMPTY_BODY_SIGNATURE);
    }

    #[test]
    fn what_it_signs_it_verifies() {
        let verifier = Verifier::new(WebhookSecret::new("a secret nobody documented"));
        let body = "{\"action\":\"opened\",\"title\":\"caf\u{e9} \u{1F680}\"}".as_bytes();

        let signature = verifier.sign(body);

        assert_eq!(verifier.verify(&signature, body), Ok(()));
        assert_eq!(
            verifier.verify(&signature, b"{}"),
            Err(SignatureError::Mismatch),
            "the signature is over the body, not a constant"
        );
    }

    #[test]
    fn a_rotated_verifier_signs_under_its_first_secret() {
        let current_first = Verifier::new(WebhookSecret::new("It's a Secret to Everybody"))
            .also(WebhookSecret::new("previous secret"));
        let previous_first = Verifier::new(WebhookSecret::new("previous secret"))
            .also(WebhookSecret::new("It's a Secret to Everybody"));

        assert_eq!(
            current_first.sign(b"Hello, World!").to_string(),
            DOCUMENTED_SIGNATURE
        );

        let under_previous = previous_first.sign(b"Hello, World!");
        assert_ne!(under_previous.to_string(), DOCUMENTED_SIGNATURE);
        assert_eq!(
            Verifier::new(WebhookSecret::new("previous secret"))
                .verify(&under_previous, b"Hello, World!"),
            Ok(()),
            "the first secret signed it, so a verifier over that secret alone accepts it"
        );
        assert_eq!(
            previous_first.verify(&under_previous, b"Hello, World!"),
            Ok(()),
            "and so does the rotated verifier itself"
        );
    }
}
